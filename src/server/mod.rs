//! Server module: TCP listener, connection accept loop, and graceful shutdown.

pub mod connection;

use std::future::Future;
use std::sync::Arc;

use tokio::net::TcpListener;
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

    /// Bind the TCP listener and run the accept loop until `Ctrl-C` or `SIGTERM`.
    pub async fn run(self) -> Result<(), ServerError> {
        self.run_with_shutdown(shutdown_signal()).await
    }

    /// Like [`run`], but accepts an arbitrary future as the shutdown signal.
    ///
    /// Useful in tests: pass a `tokio::sync::oneshot` receiver so you can
    /// trigger shutdown programmatically without sending a real OS signal.
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

        info!(addr = %self.config.addr, max_connections = self.config.max_connections, "Listening for connections");

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
async fn shutdown_signal() {
    use tokio::signal;

    #[cfg(unix)]
    {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");

        tokio::select! {
            _ = signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl-C");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use std::net::SocketAddr;

    fn test_config(port: u16) -> ServerConfig {
        let mut cfg = ServerConfig::from_env().unwrap();
        cfg.addr = SocketAddr::from(([127, 0, 0, 1], port));
        cfg.max_connections = 4;
        cfg
    }

    /// Server binds successfully and shuts down immediately via a synthetic signal.
    #[tokio::test]
    async fn server_binds_and_shuts_down() {
        // Pre-resolve a real free port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let mut cfg2 = ServerConfig::from_env().unwrap();
        cfg2.addr = addr;
        cfg2.max_connections = 4;

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        // Trigger shutdown immediately.
        let _ = tx.send(());

        let result = Server::new(cfg2)
            .run_with_shutdown(async move { let _ = rx.await; })
            .await;

        assert!(result.is_ok());
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
