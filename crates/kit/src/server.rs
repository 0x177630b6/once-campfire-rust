//! Serving an app: peer addresses for `remote_ip`, graceful shutdown on SIGINT/SIGTERM, and the
//! open-file limit many sockets need.

use std::future::Future;

use axum::Router;
use axum::extract::ConnectInfo;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower::ServiceExt;

/// Serve `app` until `shutdown` resolves, then stop accepting and let in-flight requests finish.
///
/// Like `axum::serve`, but without hyper's automatic `Date` header: Puma doesn't send one (the
/// reference's `Date` comes from Thruster in front of it, on the real clock).
pub async fn serve(listener: TcpListener, app: Router, shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    let (closing_tx, closing_rx) = watch::channel(());
    let (done_tx, done_rx) = watch::channel(());
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        let (stream, remote) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::debug!(%error, "accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    continue;
                }
            },
            _ = &mut shutdown => break,
        };
        let _ = stream.set_nodelay(true);
        let service = app.clone().map_request(move |mut request: hyper::Request<Incoming>| {
            request.extensions_mut().insert(ConnectInfo(remote));
            request.map(axum::body::Body::new)
        });
        let mut closing = closing_rx.clone();
        let done = done_rx.clone();
        tokio::spawn(async move {
            let mut builder = Builder::new(TokioExecutor::new());
            builder.http1().auto_date_header(false);
            let connection = builder.serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(service));
            let mut connection = std::pin::pin!(connection);
            tokio::select! {
                result = connection.as_mut() => {
                    if let Err(error) = result { tracing::trace!(%error, "connection failed"); }
                }
                _ = closing.changed() => {
                    connection.as_mut().graceful_shutdown();
                    let _ = connection.await;
                }
            }
            drop(done);
        });
    }
    drop(listener);
    drop(closing_rx);
    let _ = closing_tx.send(());
    drop(done_rx);
    done_tx.closed().await;
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM (what `kamal`/Docker send on stop).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

/// Raises the soft limit on open files to the hard limit, and returns the new limit. Every
/// WebSocket is a file descriptor, and containers commonly start processes with a soft limit
/// (65,536 in Docker's default, 1,024 elsewhere) far below the hard one, which would cap the
/// number of connected clients however little memory each takes.
#[cfg(unix)]
pub fn raise_open_file_limit() -> Option<u64> {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit/setrlimit read and write one `rlimit` through a valid pointer.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return None;
        }
        if limit.rlim_cur < limit.rlim_max {
            let raised = libc::rlimit { rlim_cur: limit.rlim_max, rlim_max: limit.rlim_max };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                return Some(raised.rlim_cur);
            }
        }
    }
    Some(limit.rlim_cur)
}

#[cfg(not(unix))]
pub fn raise_open_file_limit() -> Option<u64> {
    None
}

#[cfg(all(test, unix))]
mod open_file_limit_tests {
    #[test]
    fn raises_the_soft_limit_to_the_hard_one() {
        let limit = super::raise_open_file_limit().expect("getrlimit works");
        let mut now = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: getrlimit writes one `rlimit` through a valid pointer.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut now) }, 0);
        assert_eq!(now.rlim_cur, now.rlim_max);
        assert_eq!(limit, now.rlim_cur);
    }
}
