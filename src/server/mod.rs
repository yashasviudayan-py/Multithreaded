//! Server module: TCP listener, connection accept loop, and graceful shutdown.

pub mod connection;
pub mod task;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::sync::{watch, Semaphore};
use tracing::{error, info, warn};

use crate::config::ServerConfig;
use crate::middleware::RateLimiter;
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
        let listener =
            TcpListener::bind(self.config.addr)
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
        let listener =
            TcpListener::bind(self.config.addr)
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

        // Semaphore for server-wide in-flight request concurrency.  Shared
        // across all connections so one slow wave can't monopolise all workers.
        let concurrency_limiter =
            Arc::new(Semaphore::new(self.config.max_concurrent_requests));

        // Shared token-bucket rate limiter state.  Created once here and
        // passed into every connection task so all connections for the same IP
        // share the same bucket.
        let rate_limiter = Arc::new(RateLimiter::new(self.config.rate_limit_rps));
        let config = Arc::new(self.config);

        // Graceful-shutdown broadcast channel.  When the outer shutdown future
        // resolves, we send `true` so every active connection can finish its
        // current request and then close.
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // In-flight connection counter + notification for the drain wait.
        // The counter is incremented *before* spawning so there is no window
        // where a connection exists but is not counted.
        let in_flight = Arc::new(AtomicUsize::new(0));
        let all_drained = Arc::new(tokio::sync::Notify::new());

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
                                    let rl = Arc::clone(&rate_limiter);
                                    let cl = Arc::clone(&concurrency_limiter);
                                    let rx = shutdown_rx.clone();

                                    // Increment before spawn — no gap where the
                                    // task exists but is not counted.
                                    in_flight.fetch_add(1, Ordering::Relaxed);
                                    let counter = Arc::clone(&in_flight);
                                    let notify = Arc::clone(&all_drained);

                                    tokio::spawn(async move {
                                        // Permit is released when the task exits,
                                        // even on panic (Drop is always called).
                                        let _permit = permit;
                                        handle_connection(stream, peer_addr, cfg, rl, cl, rx).await;
                                        // Decrement; if we were the last one, wake the drain waiter.
                                        if counter.fetch_sub(1, Ordering::AcqRel) == 1 {
                                            notify.notify_one();
                                        }
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
                            // crash the accept loop — log, yield briefly, continue.
                            error!(err = %e, "Accept error");
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                }
            }
        }

        // ── Graceful shutdown drain ───────────────────────────────────────────
        // Signal all active connections to close after their current request.
        let _ = shutdown_tx.send(true);

        let remaining = in_flight.load(Ordering::Acquire);
        if remaining > 0 {
            info!(count = remaining, "Draining in-flight connections");

            // Register the Notify listener *before* re-checking the counter so
            // there is no race where the last task finishes between our load
            // and the .await.
            let draining = all_drained.notified();
            tokio::pin!(draining);

            if in_flight.load(Ordering::Acquire) > 0 {
                let drain_timeout =
                    Duration::from_secs(config.shutdown_drain_secs);
                match tokio::time::timeout(drain_timeout, draining).await {
                    Ok(()) => info!("All in-flight connections drained"),
                    Err(_) => warn!(
                        remaining = in_flight.load(Ordering::Relaxed),
                        "Shutdown drain timeout exceeded; forcing exit"
                    ),
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
        ServerConfig {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            workers: 1,
            max_blocking_threads: 8,
            log_level: "info".to_string(),
            static_dir: "./static".to_string(),
            rate_limit_rps: 100,
            max_connections: 4,
            tls_cert_path: None,
            tls_key_path: None,
            max_body_bytes: 4_194_304,
            keep_alive_timeout_secs: 75,
            max_concurrent_requests: 5000,
            shutdown_drain_secs: 30,
        }
    }

    /// Server signals readiness and shuts down cleanly via synthetic shutdown.
    ///
    /// Uses `run_on_listener` with a pre-bound listener — no TOCTOU race,
    /// no sleep: we wait on the ready channel instead.
    #[tokio::test]
    async fn server_binds_and_shuts_down() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let cfg = ServerConfig {
            addr,
            workers: 1,
            max_blocking_threads: 8,
            log_level: "info".to_string(),
            static_dir: "./static".to_string(),
            rate_limit_rps: 100,
            max_connections: 4,
            tls_cert_path: None,
            tls_key_path: None,
            max_body_bytes: 4_194_304,
            keep_alive_timeout_secs: 75,
            max_concurrent_requests: 5000,
            shutdown_drain_secs: 30,
        };

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (ready_tx, ready_rx) = oneshot::channel::<SocketAddr>();

        // Signal shutdown immediately so the server exits as soon as it's ready.
        shutdown_tx.send(()).unwrap();

        let result = Server::new(cfg)
            .run_on_listener(listener, Some(ready_tx), async move {
                let _ = shutdown_rx.await;
            })
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
