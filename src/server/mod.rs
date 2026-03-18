//! Server module: TCP listener, connection accept loop, and graceful shutdown.
//!
//! ## TLS support (Phase 7)
//! When `config.tls_cert_path` and `config.tls_key_path` are set the server
//! performs a TLS handshake on each accepted TCP connection before passing it
//! to [`connection::handle_connection`].  Plain HTTP and HTTPS share identical
//! middleware stacks and router logic.
//!
//! An optional HTTP→HTTPS redirect listener is spawned when
//! `config.http_redirect_port` is set; it returns `308 Permanent Redirect` for
//! every request so HTTP clients are seamlessly upgraded.

pub mod connection;
pub mod task;

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Empty;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::Request;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::sync::{watch, Semaphore};
use tracing::{debug, error, info, warn};

use crate::config::ServerConfig;
use crate::middleware::RateLimiter;
use crate::server::connection::handle_connection;
use crate::tls::{load_tls_acceptor, TlsError};

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
    /// TLS certificate/key loading failed at startup.
    #[error("TLS configuration error: {0}")]
    Tls(#[from] TlsError),
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

        // ── TLS setup ─────────────────────────────────────────────────────────
        // Load the TLS acceptor once at startup if cert+key paths are provided.
        // This surfaces cert/key errors before accepting any connections.
        let tls_acceptor = match (&self.config.tls_cert_path, &self.config.tls_key_path) {
            (Some(cert), Some(key)) => {
                let acceptor = load_tls_acceptor(cert, key)?;
                info!(addr = %addr, cert = %cert, "TLS enabled");
                Some(acceptor)
            }
            (Some(_), None) | (None, Some(_)) => {
                warn!("TLS_CERT_PATH and TLS_KEY_PATH must both be set to enable HTTPS; falling back to plain HTTP");
                None
            }
            (None, None) => None,
        };

        info!(
            addr = %addr,
            max_connections = self.config.max_connections,
            tls = tls_acceptor.is_some(),
            "Listening for connections"
        );

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
        let concurrency_limiter = Arc::new(Semaphore::new(self.config.max_concurrent_requests));

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

        // ── HTTP→HTTPS redirect server ────────────────────────────────────────
        // When TLS is active and an HTTP redirect port is configured, spawn a
        // lightweight listener that issues 308 redirects to the https:// URL.
        if tls_acceptor.is_some() {
            if let Some(redirect_port) = config.http_redirect_port {
                let redirect_addr = SocketAddr::new(addr.ip(), redirect_port);
                let https_port = addr.port();
                let rx = shutdown_rx.clone();
                tokio::spawn(run_http_redirect(redirect_addr, https_port, rx));
            }
        }

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

                                    if let Some(ref acceptor) = tls_acceptor {
                                        // TLS path: perform the handshake in the spawned task
                                        // so the accept loop is not blocked waiting for a slow client.
                                        let acceptor = acceptor.clone();
                                        tokio::spawn(async move {
                                            let _permit = permit;
                                            match acceptor.accept(stream).await {
                                                Ok(tls_stream) => {
                                                    handle_connection(tls_stream, peer_addr, cfg, rl, cl, rx).await;
                                                }
                                                Err(e) => {
                                                    warn!(peer = %peer_addr, err = %e, "TLS handshake failed");
                                                }
                                            }
                                            if counter.fetch_sub(1, Ordering::AcqRel) == 1 {
                                                notify.notify_one();
                                            }
                                        });
                                    } else {
                                        // Plain HTTP path — same as before Phase 7.
                                        tokio::spawn(async move {
                                            let _permit = permit;
                                            handle_connection(stream, peer_addr, cfg, rl, cl, rx).await;
                                            if counter.fetch_sub(1, Ordering::AcqRel) == 1 {
                                                notify.notify_one();
                                            }
                                        });
                                    }
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
                let drain_timeout = Duration::from_secs(config.shutdown_drain_secs);
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

// ── HTTP → HTTPS redirect server ──────────────────────────────────────────────

/// Bind a plain-HTTP listener and return `308 Permanent Redirect` for every
/// request, upgrading clients to `https://`.
///
/// Runs until `shutdown_rx` signals `true`.  Errors binding the listener are
/// logged but do not propagate — the HTTPS listener is unaffected.
async fn run_http_redirect(
    redirect_addr: SocketAddr,
    https_port: u16,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let listener = match TcpListener::bind(redirect_addr).await {
        Ok(l) => {
            info!(
                addr = %redirect_addr,
                https_port = https_port,
                "HTTP→HTTPS redirect server listening"
            );
            l
        }
        Err(e) => {
            error!(addr = %redirect_addr, err = %e, "Failed to bind HTTP redirect listener");
            return;
        }
    };

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("HTTP redirect server shutting down");
                    break;
                }
            }
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        tokio::spawn(serve_redirect_connection(stream, peer_addr, https_port));
                    }
                    Err(e) => {
                        error!(err = %e, "HTTP redirect accept error");
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        }
    }
}

/// Drive a single HTTP connection, returning a `308 Permanent Redirect` for
/// every request it makes.
async fn serve_redirect_connection(
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    https_port: u16,
) {
    let io = TokioIo::new(stream);
    let service = tower::service_fn(move |req: Request<Incoming>| async move {
        let host = req
            .headers()
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("localhost");

        // Strip any existing port suffix from the Host header so we can
        // substitute the HTTPS port cleanly.
        //
        // IPv6 literals look like `[::1]` or `[::1]:8080`.  We must NOT use
        // rfind(':') on them because that finds the colon inside the brackets.
        // Instead, locate the closing ']' and keep everything up to and
        // including it; the rest (if any) is the optional ":port" suffix.
        let bare_host = if host.starts_with('[') {
            // IPv6 literal: keep "[::1]" and drop any trailing ":port".
            host.find(']').map(|i| &host[..=i]).unwrap_or(host)
        } else {
            // Plain hostname or IPv4: strip the last ":port" if present.
            host.rfind(':').map(|i| &host[..i]).unwrap_or(host)
        };

        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let location = if https_port == 443 {
            format!("https://{bare_host}{path_and_query}")
        } else {
            format!("https://{bare_host}:{https_port}{path_and_query}")
        };

        Ok::<_, Infallible>(
            hyper::Response::builder()
                .status(308)
                .header("Location", &location)
                .header("Content-Length", "0")
                .body(Empty::<Bytes>::new())
                .unwrap(),
        )
    });

    let service = TowerToHyperService::new(service);

    if let Err(e) = http1::Builder::new()
        .keep_alive(false)
        .serve_connection(io, service)
        .await
    {
        debug!(peer = %peer_addr, err = %e, "HTTP redirect connection error");
    }
}

// ── OS signal helper ───────────────────────────────────────────────────────────

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
            http_redirect_port: None,
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
            http_redirect_port: None,
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
