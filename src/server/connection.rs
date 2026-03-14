//! Per-connection HTTP/1.1 handling via Hyper.
//!
//! Each accepted TCP stream is driven here inside its own `tokio::spawn` task.
//! Hyper manages keep-alive, pipelining, and framing; this module builds the
//! application [`Router`] and dispatches every request through it.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tracing::{debug, error, warn};

use crate::config::ServerConfig;
use crate::http::request::HttpRequest;
use crate::http::response::{HttpResponse, ResponseBuilder};
use crate::http::router::Router;

/// Drive a single accepted TCP connection to completion.
///
/// Wraps `stream` in a Hyper HTTP/1.1 connection, dispatches every request
/// through the application [`Router`], and logs errors.  Runs inside a
/// dedicated `tokio::spawn` task; the semaphore permit is moved in by the
/// caller.
pub async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: Arc<ServerConfig>,
) {
    debug!(peer = %peer_addr, "Connection accepted");

    // Build the router once per connection; all keep-alive requests on this
    // connection share the same Arc<Router>.
    let router = Arc::new(build_router(&config));

    let io = TokioIo::new(stream);

    let service = hyper::service::service_fn(move |req: Request<Incoming>| {
        let router = Arc::clone(&router);
        async move {
            let (parts, body) = req.into_parts();

            // Collect the request body.
            //
            // HTTP/1.1 keep-alive requires the server to fully read (or close)
            // the request body before the next request on the same connection
            // can begin.  We pass the collected bytes to HttpRequest rather
            // than discarding them, so handlers can inspect the body.
            let body_bytes: Bytes = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(e) => {
                    warn!(peer = %peer_addr, err = %e, "Failed to collect request body");
                    return Ok::<HttpResponse, Infallible>(ResponseBuilder::bad_request().empty());
                }
            };

            let request = HttpRequest::from_parts(parts, body_bytes);
            let response = router.dispatch(request).await;
            Ok::<HttpResponse, Infallible>(response)
        }
    });

    match http1::Builder::new()
        .keep_alive(true)
        .serve_connection(io, service)
        .await
    {
        Ok(()) => debug!(peer = %peer_addr, "Connection closed cleanly"),
        Err(e) => {
            if e.is_incomplete_message() {
                debug!(peer = %peer_addr, "Client disconnected mid-request");
            } else {
                error!(peer = %peer_addr, err = %e, "HTTP connection error");
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
fn build_router(_cfg: &ServerConfig) -> Router {
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

    router
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
}
