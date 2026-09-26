//! Serving an app: peer addresses for `remote_ip`, and graceful shutdown on SIGINT/SIGTERM.

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
