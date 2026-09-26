//! Serving an app: peer addresses for `remote_ip`, and graceful shutdown on SIGINT/SIGTERM.

use std::future::Future;
use std::net::SocketAddr;

use axum::Router;
use tokio::net::TcpListener;

/// Serve `app` until `shutdown` resolves, then stop accepting and let in-flight requests finish.
pub async fn serve(listener: TcpListener, app: Router, shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown).await
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
