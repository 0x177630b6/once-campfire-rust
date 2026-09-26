//! One writer thread that owns the write connection and takes a bounded queue of work, plus
//! a pool of reader connections. Each write runs in `BEGIN IMMEDIATE`
//! (`default_transaction_mode: immediate` in `reference/config/database.yml`), then its
//! after-commit work runs in order, outside the transaction, the way Active Record runs
//! `after_commit` callbacks.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::{mpsc, oneshot};

use crate::error::{Error, Result};
use crate::events::{Event, EventSink, NullSink};
use crate::rich_text::{BasicRichText, RichText};
use crate::schema;
use crate::time::{Clock, SystemClock, Timestamp};

/// Everything models need besides the connection: the clock, where side effects go, and
/// the Action Text adapter.
#[derive(Clone)]
pub struct Env {
    pub clock: Arc<dyn Clock>,
    pub sink: Arc<dyn EventSink>,
    pub rich_text: Arc<dyn RichText>,
    /// BCrypt cost for `has_secure_password`. Rails uses `BCrypt::Engine.cost` (12), or
    /// `MIN_COST` (4) in the test environment.
    pub bcrypt_cost: u32,
}

impl Default for Env {
    fn default() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            sink: Arc::new(NullSink),
            rich_text: Arc::new(BasicRichText),
            bcrypt_cost: 12,
        }
    }
}

impl Env {
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }
}

type AfterCommitHook = Box<dyn FnOnce(&mut Tx<'_>) -> Result<()> + Send>;

enum AfterCommit {
    Hook(AfterCommitHook),
    Event(Event),
}

/// A write in progress: the writer connection inside a transaction (or, while after-commit
/// work runs, outside one), the environment, and the queued after-commit work.
pub struct Tx<'c> {
    conn: &'c Connection,
    env: &'c Env,
    in_transaction: bool,
    after_commit: Vec<AfterCommit>,
}

impl<'c> Tx<'c> {
    pub fn conn(&self) -> &'c Connection {
        self.conn
    }

    pub fn env(&self) -> &'c Env {
        self.env
    }

    /// `Time.current`
    pub fn now(&self) -> Timestamp {
        self.env.now()
    }

    pub fn rich_text(&self) -> &'c dyn RichText {
        &*self.env.rich_text
    }

    /// Emits an event right away, even though the transaction may still roll back, for the
    /// side effects Rails performs mid-transaction.
    pub fn emit_now(&self, event: Event) {
        self.env.sink.emit(event);
    }

    /// Emits an event once the transaction commits (`after_commit`), or right away when
    /// already running after commit.
    pub fn emit_after_commit(&mut self, event: Event) {
        if self.in_transaction {
            self.after_commit.push(AfterCommit::Event(event));
        } else {
            self.env.sink.emit(event);
        }
    }

    /// Queues database work to run after commit, in its own implicit transaction.
    pub fn after_commit(&mut self, hook: impl FnOnce(&mut Tx<'_>) -> Result<()> + Send + 'static) {
        if self.in_transaction {
            self.after_commit.push(AfterCommit::Hook(Box::new(hook)));
        } else {
            let mut tx = Tx {
                conn: self.conn,
                env: self.env,
                in_transaction: false,
                after_commit: Vec::new(),
            };
            if let Err(error) = hook(&mut tx) {
                tracing::error!(%error, "after_commit hook failed");
            }
        }
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }
}

/// Runs `f` in `BEGIN IMMEDIATE`, commits, then runs the after-commit queue. An error from
/// `f` rolls back and discards the queue. An error from an after-commit hook is returned
/// after the rest of the queue has run (Rails raises it from the save that committed).
pub fn run_write<T>(
    conn: &Connection,
    env: &Env,
    f: impl FnOnce(&mut Tx<'_>) -> Result<T>,
) -> Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE TRANSACTION")?;
    let mut tx = Tx {
        conn,
        env,
        in_transaction: true,
        after_commit: Vec::new(),
    };
    let value = match f(&mut tx) {
        Ok(value) => value,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK TRANSACTION");
            return Err(error);
        }
    };
    if let Err(error) = conn.execute_batch("COMMIT TRANSACTION") {
        let _ = conn.execute_batch("ROLLBACK TRANSACTION");
        return Err(error.into());
    }

    let mut queue = std::mem::take(&mut tx.after_commit);
    let mut first_error = None;
    let mut after = Tx {
        conn,
        env,
        in_transaction: false,
        after_commit: Vec::new(),
    };
    for item in queue.drain(..) {
        match item {
            AfterCommit::Event(event) => env.sink.emit(event),
            AfterCommit::Hook(hook) => {
                if let Err(error) = hook(&mut after) {
                    tracing::error!(%error, "after_commit hook failed");
                    first_error.get_or_insert(error);
                }
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(value),
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub path: PathBuf,
    pub readers: usize,
    /// Bound on queued writes; senders wait when it's full.
    pub write_queue: usize,
    /// Load the schema into an empty database (`db:prepare`).
    pub prepare: bool,
    /// `ar_internal_metadata.environment` when preparing.
    pub environment: String,
}

impl Config {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            readers: 8,
            write_queue: 256,
            prepare: true,
            environment: "production".into(),
        }
    }
}

type Job = Box<dyn FnOnce(&Connection, &Env) + Send>;

/// The database handle. Cheap to clone.
#[derive(Clone)]
pub struct Database {
    writer: mpsc::Sender<Job>,
    readers: Arc<ReaderPool>,
    env: Env,
    path: PathBuf,
}

impl Database {
    pub fn open(config: Config, env: Env) -> Result<Self> {
        let mut conn = open_connection(&config.path, false)?;
        if config.prepare {
            schema::prepare(&mut conn, &config.environment, &*env.clock)?;
        }

        let (sender, mut receiver) = mpsc::channel::<Job>(config.write_queue.max(1));
        let writer_env = env.clone();
        std::thread::Builder::new()
            .name("campfire-db-writer".into())
            .spawn(move || {
                while let Some(job) = receiver.blocking_recv() {
                    // A panicking write must not take the writer down with it.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        job(&conn, &writer_env)
                    }));
                    if outcome.is_err() && !conn.is_autocommit() {
                        let _ = conn.execute_batch("ROLLBACK TRANSACTION");
                    }
                }
            })
            .map_err(|e| Error::Other(e.to_string()))?;

        let readers = (0..config.readers.max(1))
            .map(|_| open_connection(&config.path, true))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            writer: sender,
            readers: Arc::new(ReaderPool::new(readers)),
            env,
            path: config.path,
        })
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` as one immediate transaction on the writer thread.
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Tx<'_>) -> Result<T> + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        self.writer
            .send(Box::new(move |conn, env| {
                let _ = reply.send(run_write(conn, env, f));
            }))
            .await
            .map_err(|_| Error::WriterGone)?;
        response.await.map_err(|_| Error::WriterGone)?
    }

    /// [`Database::write`] for synchronous callers (not from inside an async task).
    pub fn write_blocking<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Tx<'_>) -> Result<T> + Send + 'static,
    {
        let (reply, response) = oneshot::channel();
        self.writer
            .blocking_send(Box::new(move |conn, env| {
                let _ = reply.send(run_write(conn, env, f));
            }))
            .map_err(|_| Error::WriterGone)?;
        response.blocking_recv().map_err(|_| Error::WriterGone)?
    }

    /// Runs `f` on a reader connection, on the blocking pool.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let readers = self.readers.clone();
        tokio::task::spawn_blocking(move || readers.with(f))
            .await
            .map_err(|e| Error::Other(e.to_string()))?
    }

    /// [`Database::read`] for synchronous callers.
    pub fn read_blocking<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.readers.with(f)
    }
}

fn open_connection(path: &Path, reader: bool) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(path, flags)?;
    schema::configure_connection(&conn)?;
    if reader {
        conn.pragma_update(None, "query_only", true)?;
    }
    Ok(conn)
}

struct ReaderPool {
    idle: Mutex<Vec<Connection>>,
    available: Condvar,
}

impl ReaderPool {
    fn new(connections: Vec<Connection>) -> Self {
        Self {
            idle: Mutex::new(connections),
            available: Condvar::new(),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = {
            let mut idle = self.idle.lock().unwrap();
            loop {
                if let Some(conn) = idle.pop() {
                    break conn;
                }
                idle = self.available.wait(idle).unwrap();
            }
        };
        let result = f(&conn);
        self.idle.lock().unwrap().push(conn);
        self.available.notify_one();
        result
    }
}
