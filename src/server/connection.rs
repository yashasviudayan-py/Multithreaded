//! Per-connection HTTP/1.1 handling via Hyper.
//!
//! Each accepted TCP stream is driven here inside its own `tokio::spawn` task.
//! Hyper manages keep-alive, pipelining, and framing; this module builds the
//! application [`Router`] and dispatches every request through it via a
//! composed Tower middleware stack:
//!
//! ```text
//! LoggingLayer          ←  outermost (measures full latency, logs status)
//!   └─ RateLimiterLayer        (per-IP token-bucket; returns 429 on exhaustion)
//!        └─ ConcurrencyLimiterLayer  (global cap; returns 503 when full)
//!             └─ service_fn          (collect body, enforce size limit, dispatch)
//! ```
//!
//! ## Graceful shutdown
//! The accept loop passes a [`watch::Receiver<bool>`] that becomes `true` when
//! a shutdown signal is received.  `handle_connection` detects this inside its
//! `select!` loop and calls [`hyper::server::conn::http1::Connection::graceful_shutdown`],
//! which sends `Connection: close` on the next response so the client knows not
//! to reuse the connection.  The loop then runs to completion.
//!
//! ## Keep-alive timeout
//! A [`tokio::time::sleep`] timer is armed when the connection is established.
//! If it fires before the connection closes naturally the task exits, which
//! drops the TCP stream and lets the OS close the socket.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpStream;
use tokio::sync::{watch, Semaphore};
use tower::ServiceBuilder;
use tracing::{debug, error, warn};

use crate::config::ServerConfig;
use crate::http::request::HttpRequest;
use crate::http::response::{HttpResponse, ResponseBuilder};
use crate::http::router::Router;
use crate::middleware::{ConcurrencyLimiterLayer, LoggingLayer, RateLimiter, RateLimiterLayer};
use crate::server::task::run_blocking;
use crate::static_files;

/// Drive a single accepted TCP connection to completion.
///
/// Builds the Tower middleware stack and drives the Hyper HTTP/1.1 connection
/// state machine.  The [`watch::Receiver`] is used to signal graceful shutdown
/// across all active connections simultaneously.
pub async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: Arc<ServerConfig>,
    rate_limiter: Arc<RateLimiter>,
    concurrency_limiter: Arc<Semaphore>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    debug!(peer = %peer_addr, "Connection accepted");

    // Build the router once per connection; all keep-alive requests on this
    // connection share the same Arc<Router>.
    let router = Arc::new(build_router(&config));
    let max_body = config.max_body_bytes;

    // ── Inner service: body collection + dispatch ──────────────────────────
    // Use tower::service_fn (not hyper::service::service_fn) so the result
    // implements tower::Service and can be composed with Tower middleware layers.
    let inner = tower::service_fn(move |req: Request<Incoming>| {
        let router = Arc::clone(&router);
        async move {
            let (parts, body) = req.into_parts();

            // Enforce body size limit before collecting.  Check Content-Length
            // for an early, cheap rejection of well-behaved clients.
            if let Some(cl) = parts
                .headers
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<usize>().ok())
            {
                if cl > max_body {
                    return Ok::<HttpResponse, Infallible>(
                        ResponseBuilder::new(StatusCode::PAYLOAD_TOO_LARGE)
                            .text("413 Payload Too Large\n"),
                    );
                }
            }

            // HTTP/1.1 keep-alive requires the server to fully consume the
            // request body before the next request on the same connection can
            // begin.  Collect and enforce the runtime limit.
            let body_bytes: Bytes = match body.collect().await {
                Ok(collected) => {
                    let bytes = collected.to_bytes();
                    if bytes.len() > max_body {
                        return Ok(ResponseBuilder::new(StatusCode::PAYLOAD_TOO_LARGE)
                            .text("413 Payload Too Large\n"));
                    }
                    bytes
                }
                Err(e) => {
                    warn!(peer = %peer_addr, err = %e, "Failed to collect request body");
                    return Ok(ResponseBuilder::bad_request().empty());
                }
            };

            let request = HttpRequest::from_parts(parts, body_bytes);
            let response = router.dispatch(request).await;
            Ok::<HttpResponse, Infallible>(response)
        }
    });

    // ── Compose Tower middleware stack ─────────────────────────────────────
    // Request flow: LoggingService → RateLimiterService → ConcurrencyLimiterService → inner
    let tower_stack = ServiceBuilder::new()
        .layer(LoggingLayer::new(peer_addr))
        .layer(RateLimiterLayer::new(rate_limiter, peer_addr.ip()))
        .layer(ConcurrencyLimiterLayer::new(concurrency_limiter))
        .service(inner);

    let service = TowerToHyperService::new(tower_stack);
    let io = TokioIo::new(stream);

    // ── Drive connection with keep-alive timeout + graceful shutdown ───────
    let conn = http1::Builder::new()
        .keep_alive(true)
        .serve_connection(io, service);
    tokio::pin!(conn);

    // Keep-alive idle timeout: drop the connection if it sits idle for too long.
    let ka_timeout =
        tokio::time::sleep(Duration::from_secs(config.keep_alive_timeout_secs));
    tokio::pin!(ka_timeout);

    // Track whether we have already initiated graceful shutdown on this
    // connection (prevents calling graceful_shutdown() more than once).
    let mut graceful = false;

    loop {
        tokio::select! {
            biased;

            result = &mut conn => {
                match result {
                    Ok(()) => debug!(peer = %peer_addr, "Connection closed cleanly"),
                    Err(e) if e.is_incomplete_message() => {
                        debug!(peer = %peer_addr, "Client disconnected mid-request")
                    }
                    Err(e) => error!(peer = %peer_addr, err = %e, "HTTP connection error"),
                }
                break;
            }

            // Listen for the server-wide graceful-shutdown broadcast.
            _ = shutdown_rx.changed(), if !graceful => {
                if *shutdown_rx.borrow() {
                    graceful = true;
                    debug!(peer = %peer_addr, "Graceful shutdown: sending Connection: close");
                    conn.as_mut().graceful_shutdown();
                }
                // If the value changed but is still false (shouldn't happen),
                // just continue polling.
            }

            // Keep-alive idle timeout: close the connection if unused too long.
            _ = &mut ka_timeout, if !graceful => {
                debug!(peer = %peer_addr, "Keep-alive timeout — closing idle connection");
                break;
            }
        }
    }
}

/// Build the application router.
///
/// Called once per accepted connection.  Add routes here as new endpoints are
/// developed.  The router is wrapped in an `Arc` by `handle_connection` so it
/// is shared across all keep-alive requests on the same connection without
/// cloning.
pub(crate) fn build_router(cfg: &ServerConfig) -> Router {
    let mut router = Router::new();

    router.get("/", |_req| async {
        ResponseBuilder::ok().text("Hello from rust-highperf-server\n")
    });

    router.get("/health", |_req| async {
        ResponseBuilder::ok().text("ok\n")
    });

    // Example of path-parameter routing added in Phase 2.
    router.get("/echo/:message", |req| async move {
        let msg = req.path_param("message").unwrap_or("").to_string();
        ResponseBuilder::ok().text(format!("{msg}\n"))
    });

    // CPU-bound demo route added in Phase 3.
    //
    // Computes the n-th Fibonacci number on the blocking thread pool so the
    // async workers stay free.  Caps `n` at 50 to prevent runaway computation
    // (fib(50) already takes ~100 ms with the naive recursive algorithm, which
    // is enough to demonstrate the blocking pool without overloading tests).
    router.get("/fib/:n", |req| async move {
        let n: u64 = req
            .path_param("n")
            .and_then(|v| v.parse().ok())
            .unwrap_or(10)
            .min(50);

        match run_blocking(move || fib(n)).await {
            Ok(result) => ResponseBuilder::ok().text(format!("{result}\n")),
            Err(e) => {
                tracing::error!(err = %e, "Blocking fib task failed");
                ResponseBuilder::internal_error().empty()
            }
        }
    });

    // Static file serving added in Phase 4.
    //
    // Files are served from `config.static_dir` relative to the server's
    // working directory.  Path traversal attacks are blocked in `serve_file`.
    let static_dir = PathBuf::from(&cfg.static_dir);
    router.get("/static/*filepath", move |req| {
        let base = static_dir.clone();
        async move {
            let filepath = req.path_param("filepath").unwrap_or("").to_string();
            static_files::serve_file(&base, &filepath).await
        }
    });

    router
}

/// Compute the n-th Fibonacci number iteratively.
///
/// Intentionally CPU-bound (for large n) to demonstrate [`run_blocking`]
/// keeping async workers free.  Safe for all u64 n (returns 0 for n == 0,
/// wraps on overflow beyond fib(93) but the `/fib/:n` route caps at 50).
fn fib(n: u64) -> u64 {
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 0..n {
        (a, b) = (b, a.wrapping_add(b));
    }
    a
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::{Method, StatusCode};

    fn test_config() -> Arc<ServerConfig> {
        Arc::new(ServerConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
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
        })
    }

    fn make_req(method: Method, uri: &str) -> HttpRequest {
        let (parts, _) = hyper::Request::builder()
            .method(method)
            .uri(uri)
            .body(())
            .unwrap()
            .into_parts();
        HttpRequest::from_parts(parts, Bytes::new())
    }

    async fn body_str(resp: HttpResponse) -> String {
        let bytes: Bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn health_route_returns_ok() {
        let cfg = test_config();
        let router = build_router(&cfg);
        let resp = router.dispatch(make_req(Method::GET, "/health")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_str(resp).await, "ok\n");
    }

    #[tokio::test]
    async fn root_route_returns_server_name() {
        let cfg = test_config();
        let router = build_router(&cfg);
        let resp = router.dispatch(make_req(Method::GET, "/")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("rust-highperf-server"));
    }

    #[tokio::test]
    async fn echo_path_param_returned() {
        let cfg = test_config();
        let router = build_router(&cfg);
        let resp = router.dispatch(make_req(Method::GET, "/echo/world")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_str(resp).await, "world\n");
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let cfg = test_config();
        let router = build_router(&cfg);
        let resp = router.dispatch(make_req(Method::GET, "/not-a-route")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn fib_known_values() {
        assert_eq!(fib(0), 0);
        assert_eq!(fib(1), 1);
        assert_eq!(fib(10), 55);
        assert_eq!(fib(20), 6765);
    }

    #[tokio::test]
    async fn fib_route_returns_correct_value() {
        let cfg = test_config();
        let router = build_router(&cfg);
        let resp = router.dispatch(make_req(Method::GET, "/fib/10")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_str(resp).await, "55\n");
    }

    #[tokio::test]
    async fn fib_route_caps_at_50() {
        let cfg = test_config();
        let router = build_router(&cfg);
        // n=999 should be capped to 50 → fib(50) = 12586269025
        let resp = router.dispatch(make_req(Method::GET, "/fib/999")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_str(resp).await, "12586269025\n");
    }

    #[tokio::test]
    async fn response_has_server_header() {
        let cfg = test_config();
        let router = build_router(&cfg);
        let resp = router.dispatch(make_req(Method::GET, "/health")).await;
        assert_eq!(
            resp.headers().get("server").unwrap(),
            "rust-highperf-server/0.1"
        );
    }

    #[tokio::test]
    async fn static_route_returns_404_for_missing_file() {
        let cfg = test_config();
        let router = build_router(&cfg);
        let resp = router
            .dispatch(make_req(Method::GET, "/static/nonexistent.txt"))
            .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
