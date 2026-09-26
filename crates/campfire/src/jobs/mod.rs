//! The in-process job runner that replaces Resque (see plans/rust-conversion.md, "Jobs").
//!
//! Models emit [`Event`]s at the point Rails would `perform_later` (the database's
//! [`EventSink`]); [`Jobs`] puts them on a **bounded** queue without blocking the writer thread,
//! and the runner performs them with a per-kind concurrency bound. Nothing retries
//! (`retry_on` is commented out in `reference/app/jobs/application_job.rb`); a failure is logged.
//! Queued work is lost if the process crashes, which the plan accepts. On shutdown the runner
//! stops taking new work, performs what's queued and waits for it up to a deadline.
//!
//! Handlers are looked up in a [`Registry`]. Core registers `RemoveBannedContent` and
//! `PurgeBlob`; integrations register `PushMessage` and `DeliverWebhook` through
//! `crate::integrations::register_jobs`. `DisconnectUser` is not a job in Rails (it's a
//! synchronous Action Cable broadcast), so it goes straight to the cable server.

// The frame later controller ports build on; parts are unused until they land.
#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use campfire_db::{Event, EventSink};
use futures_util::future::BoxFuture;
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::app::{App, Cable};

/// How many jobs may wait in the queue before new ones are dropped (and logged).
pub const QUEUE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    PushMessage,
    DeliverWebhook,
    RemoveBannedContent,
    DisconnectUser,
    PurgeBlob,
    /// Work enqueued with [`Jobs::perform_later`].
    AdHoc,
}

impl JobKind {
    pub fn of(event: &Event) -> Self {
        match event {
            Event::PushMessage { .. } => JobKind::PushMessage,
            Event::DeliverWebhook { .. } => JobKind::DeliverWebhook,
            Event::RemoveBannedContent { .. } => JobKind::RemoveBannedContent,
            Event::DisconnectUser { .. } => JobKind::DisconnectUser,
            Event::PurgeBlob { .. } => JobKind::PurgeBlob,
        }
    }

    /// The Rails job class, for logs.
    pub fn name(self) -> &'static str {
        match self {
            JobKind::PushMessage => "Room::PushMessageJob",
            JobKind::DeliverWebhook => "Bot::WebhookJob",
            JobKind::RemoveBannedContent => "RemoveBannedContentJob",
            JobKind::DisconnectUser => "ActionCable::RemoteConnections#disconnect",
            JobKind::PurgeBlob => "ActiveStorage::PurgeJob",
            JobKind::AdHoc => "AdHoc",
        }
    }
}

/// Performs one kind of job.
#[async_trait::async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn perform(&self, app: App, event: Event) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
impl<F, Fut> Handler for F
where
    F: Fn(App, Event) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    async fn perform(&self, app: App, event: Event) -> anyhow::Result<()> {
        self(app, event).await
    }
}

/// Which handler performs which kind of job.
#[derive(Default)]
pub struct Registry {
    handlers: HashMap<JobKind, Arc<dyn Handler>>,
}

impl Registry {
    /// The jobs the app core performs itself.
    pub fn with_core_jobs() -> Self {
        let mut registry = Self::default();
        registry.handle(JobKind::RemoveBannedContent, remove_banned_content);
        registry.handle(JobKind::PurgeBlob, purge_blob);
        registry
    }

    pub fn handle(&mut self, kind: JobKind, handler: impl Handler) {
        self.handlers.insert(kind, Arc::new(handler));
    }

    fn get(&self, kind: JobKind) -> Option<Arc<dyn Handler>> {
        self.handlers.get(&kind).cloned()
    }
}

enum Work {
    Event(Event),
    AdHoc(&'static str, BoxFuture<'static, anyhow::Result<()>>),
}

/// The enqueueing side: the database's event sink, and `perform_later` for everything else.
/// Cheap to clone.
#[derive(Clone)]
pub struct Jobs {
    queue: mpsc::Sender<Work>,
    cable: Arc<OnceLock<Cable>>,
}

impl Jobs {
    /// The queue and its receiving end, which [`start`] turns into the runner.
    pub fn new(capacity: usize) -> (Self, Queue) {
        let (queue, receiver) = mpsc::channel(capacity.max(1));
        (Self { queue, cable: Arc::new(OnceLock::new()) }, Queue { receiver })
    }

    /// Enqueues best-effort work (`SomeJob.perform_later`). Dropped with an error log when the
    /// queue is full.
    pub fn perform_later(&self, name: &'static str, work: impl Future<Output = anyhow::Result<()>> + Send + 'static) {
        self.enqueue(Work::AdHoc(name, Box::pin(work)));
    }

    fn enqueue(&self, work: Work) {
        let name = match &work {
            Work::Event(event) => JobKind::of(event).name(),
            Work::AdHoc(name, _) => name,
        };
        match self.queue.try_send(work) {
            Ok(()) => tracing::debug!(job = name, "enqueued"),
            Err(mpsc::error::TrySendError::Full(_)) => tracing::error!(job = name, "job queue is full, dropping job"),
            Err(mpsc::error::TrySendError::Closed(_)) => tracing::warn!(job = name, "job runner stopped, dropping job"),
        }
    }

    fn set_cable(&self, cable: Cable) {
        let _ = self.cable.set(cable);
    }
}

impl EventSink for Jobs {
    fn emit(&self, event: Event) {
        match event {
            // `ActionCable.server.remote_connections.where(current_user: user).disconnect`: a
            // pub/sub broadcast in Rails, done right away. Before boot finishes there are no
            // connections to disconnect.
            Event::DisconnectUser { user_id, reconnect } => {
                if let Some(cable) = self.cable.get() {
                    crate::channels::revocation::disconnect_user(cable, user_id, reconnect);
                }
            }
            event => self.enqueue(Work::Event(event)),
        }
    }
}

/// The receiving end of the queue, until the runner starts.
pub struct Queue {
    receiver: mpsc::Receiver<Work>,
}

/// The running job runner.
pub struct Runner {
    shutdown: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Runner {
    /// Stops taking new work, performs what's already queued, and waits for running jobs until
    /// `deadline`, after which they're abandoned (logged).
    pub async fn shutdown(self, deadline: Duration) {
        self.shutdown.notify_one();
        if tokio::time::timeout(deadline, self.task).await.is_err() {
            tracing::warn!("jobs still running at shutdown were abandoned");
        }
    }
}

/// Starts performing queued jobs. `concurrency` bounds each kind of job separately.
pub fn start(queue: Queue, app: App, registry: Registry, concurrency: usize) -> Runner {
    app.jobs.set_cable(app.cable.clone());
    let shutdown = Arc::new(Notify::new());
    let task = tokio::spawn(run(queue.receiver, app, Arc::new(registry), concurrency.max(1), shutdown.clone()));
    Runner { shutdown, task }
}

async fn run(mut receiver: mpsc::Receiver<Work>, app: App, registry: Arc<Registry>, concurrency: usize, shutdown: Arc<Notify>) {
    let mut limits: HashMap<JobKind, Arc<Semaphore>> = HashMap::new();
    let mut running = JoinSet::new();
    loop {
        let work = tokio::select! {
            work = receiver.recv() => work,
            _ = shutdown.notified() => {
                receiver.close();
                receiver.recv().await
            }
        };
        let Some(work) = work else { break };
        let kind = match &work {
            Work::Event(event) => JobKind::of(event),
            Work::AdHoc(..) => JobKind::AdHoc,
        };
        let limit = limits.entry(kind).or_insert_with(|| Arc::new(Semaphore::new(concurrency))).clone();
        let Ok(permit) = limit.acquire_owned().await else { continue };
        let app = app.clone();
        let registry = registry.clone();
        running.spawn(async move {
            perform(app, &registry, work).await;
            drop(permit);
        });
        while running.try_join_next().is_some() {}
    }
    while running.join_next().await.is_some() {}
}

async fn perform(app: App, registry: &Registry, work: Work) {
    let (name, result) = match work {
        Work::AdHoc(name, future) => (name, future.await),
        Work::Event(event) => {
            let kind = JobKind::of(&event);
            match registry.get(kind) {
                Some(handler) => (kind.name(), handler.perform(app, event).await),
                None => {
                    tracing::warn!(job = kind.name(), ?event, "no handler registered, skipping job");
                    return;
                }
            }
        }
    };
    match result {
        Ok(()) => tracing::info!(job = name, "performed"),
        Err(error) => tracing::error!(job = name, %error, "job failed"),
    }
}

/// `RemoveBannedContentJob`: `user.remove_banned_content`, which destroys each of the user's
/// messages (each in its own transaction) and broadcasts its removal
/// (`reference/app/models/user/bannable.rb`, `Message::Broadcasts#broadcast_remove`).
async fn remove_banned_content(app: App, event: Event) -> anyhow::Result<()> {
    let Event::RemoveBannedContent { user_id } = event else { return Ok(()) };
    let messages = app.db.read(move |conn| campfire_db::Message::by_creator(conn, user_id)).await?;
    for message in messages {
        let (removed, room_id) = (message.clone(), message.room_id);
        app.db.write(move |tx| message.destroy(tx)).await?;
        let room = app.db.read(move |conn| campfire_db::Room::find(conn, room_id)).await?;
        app.broadcasts.message_remove(&room, &removed);
    }
    Ok(())
}

/// `ActiveStorage::PurgeJob`
async fn purge_blob(app: App, event: Event) -> anyhow::Result<()> {
    let Event::PurgeBlob { blob_id } = event else { return Ok(()) };
    crate::active_storage::purge(&app, blob_id).await
}
