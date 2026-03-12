//! Server module: TCP listener, connection accept loop, and graceful shutdown.

pub mod connection;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

use crate::config::ServerConfig;
use crate::server::connection::handle_connection;

/// Errors that can occur while binding or running the server.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Failed to bind to {addr}: {source}")]
    Bind {
        addr: std::net::SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("Accept loop I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// The HTTP server: owns configuration and orchestrates accept/shutdown.
pub struct Server {
    config: ServerConfig,
}

impl Server {
    /// Create a new server with the given configuration.
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }

    /// Bind to `config.addr` and run the accept loop until `Ctrl-C` or `SIGTERM`.
    pub async fn run(self) -> Result<(), ServerError> {
        let listener = TcpListener::bind(self.config.addr)
            .await
            .map_err(|e| ServerError::Bind {
                addr: self.config.addr,
                source: e,
            })?;
        self.accept_loop(listener, None, shutdown_signal()).await
    }

    /// Like [`run`], but with a custom shutdown future.
    ///
    /// Binds `config.addr` internally, then runs the accept loop until `shutdown`
    /// resolves.  Useful when you need custom shutdown logic (e.g., draining
    /// in-flight work before exit) without the OS signal handler.
    pub async fn run_with_shutdown(
        self,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ServerError> {
        let listener = TcpListener::bind(self.config.addr)
            .await
            .map_err(|e| ServerError::Bind {
                addr: self.config.addr,
                source: e,
            })?;
        self.accept_loop(listener, None, shutdown).await
    }

    /// Run on a **pre-bound** listener with an optional ready-signal channel.
    ///
    /// This is the preferred entry point for tests: the caller binds the port
    /// and keeps the `TcpListener` alive, eliminating the TOCTOU race that
    /// would exist if we bound, dropped, and re-bound on the same port.
    ///
    /// `ready_tx` — if provided — is sent the bound `SocketAddr` immediately
    /// before the accept loop starts, letting callers synchronise on server
    /// readiness without a sleep.
    pub async fn run_on_listener(
        self,
        listener: TcpListener,
        ready_tx: Option<oneshot::Sender<SocketAddr>>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ServerError> {
        self.accept_loop(listener, ready_tx, shutdown).await
    }

    /// Internal accept loop shared by all public entry points.
    async fn accept_loop(
        self,
        listener: TcpListener,
        ready_tx: Option<oneshot::Sender<SocketAddr>>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ServerError> {
        let addr = listener.local_addr()?;

        info!(addr = %addr, max_connections = self.config.max_connections, "Listening for connections");

        // Notify tests (or any orchestrator) that we are ready to accept.
        if let Some(tx) = ready_tx {
            // Receiver may already be gone if the caller timed out — that's fine.
            let _ = tx.send(addr);
        }

        // Semaphore enforces the connection cap.  Each spawned task holds one
        // permit for the lifetime of the connection; `try_acquire_owned` is
        // non-blocking — if we're at the limit we drop the socket immediately
        // rather than queuing indefinitely (DoS mitigation).
        let connection_limit = Arc::new(Semaphore::new(self.config.max_connections));
        let config = Arc::new(self.config);

        // Pin the shutdown future so we can poll it across loop iterations
        // without re-registering OS signal handlers each time.
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                // `biased` ensures the shutdown branch is polled first every
                // iteration — prevents starvation of the signal under high load.
                biased;

                _ = &mut shutdown => {
                    info!("Shutdown signal received — stopping accept loop");
                    break;
                }

                result = listener.accept() => {
                    match result {
                        Ok((stream, peer_addr)) => {
                            // Reduce latency for small responses (ACKs sent immediately).
                            if let Err(e) = stream.set_nodelay(true) {
                                warn!(peer = %peer_addr, err = %e, "Failed to set TCP_NODELAY");
                            }

                            match Arc::clone(&connection_limit).try_acquire_owned() {
                                Ok(permit) => {
                                    let cfg = Arc::clone(&config);
                                    tokio::spawn(async move {
                                        // Permit is released when the task exits,
                                        // even on panic (Drop is always called).
                                        let _permit = permit;
                                        handle_connection(stream, peer_addr, cfg).await;
                                    });
                                }
                                Err(_) => {
                                    // Stream drops here → kernel sends TCP FIN.
                                    warn!(
                                        peer = %peer_addr,
                                        limit = config.max_connections,
                                        "Connection limit reached — dropping connection"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            // Transient OS errors (EMFILE, ENFILE) must not
                            // crash the accept loop — log and continue.
                            error!(err = %e, "Accept error");
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Resolves on the first of `SIGINT` (`Ctrl-C`) or `SIGTERM`.
///
/// Falls back gracefully if signal registration fails instead of panicking:
/// a failed SIGTERM handler is logged as a warning and only SIGINT is used.
async fn shutdown_signal() {
    use tokio::signal;

    #[cfg(unix)]
    {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(e) => {
                warn!(
                    err = %e,
                    "Could not register SIGTERM handler; only SIGINT (Ctrl-C) will trigger shutdown"
                );
                // Best-effort: if ctrl_c also fails there's nothing we can do.
                let _ = signal::ctrl_c().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        // On non-Unix platforms only Ctrl-C is available.
        if let Err(e) = signal::ctrl_c().await {
            warn!(err = %e, "Could not listen for Ctrl-C shutdown signal");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use std::net::SocketAddr;
    use tokio::sync::oneshot;

    fn test_config(port: u16) -> ServerConfig {
        let mut cfg = ServerConfig::from_env().unwrap();
        cfg.addr = SocketAddr::from(([127, 0, 0, 1], port));
        cfg.max_connections = 4;
        cfg
    }

    /// Server signals readiness and shuts down cleanly via synthetic shutdown.
    ///
    /// Uses `run_on_listener` with a pre-bound listener — no TOCTOU race,
    /// no sleep: we wait on the ready channel instead.
    #[tokio::test]
    async fn server_binds_and_shuts_down() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut cfg = ServerConfig::from_env().unwrap();
        cfg.addr = addr;
        cfg.max_connections = 4;

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (ready_tx, ready_rx) = oneshot::channel::<SocketAddr>();

        // Signal shutdown immediately so the server exits as soon as it's ready.
        shutdown_tx.send(()).unwrap();

        let result = Server::new(cfg)
            .run_on_listener(
                listener,
                Some(ready_tx),
                async move { let _ = shutdown_rx.await; },
            )
            .await;

        assert!(result.is_ok());
        // Server should have sent its bound address before entering the loop.
        assert_eq!(ready_rx.await.unwrap(), addr);
    }

    /// `Server::new` stores the config unchanged.
    #[test]
    fn new_stores_config() {
        let cfg = test_config(9999);
        let server = Server::new(cfg.clone());
        assert_eq!(server.config.addr, cfg.addr);
        assert_eq!(server.config.max_connections, cfg.max_connections);
    }
}
