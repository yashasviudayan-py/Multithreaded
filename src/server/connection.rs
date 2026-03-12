//! Per-connection HTTP/1.1 handling via Hyper.
//!
//! Each accepted TCP stream is driven here inside its own `tokio::spawn` task.
//! The Hyper connection manages keep-alive, pipelining, and framing; this
//! module owns only the service function and error reporting.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tracing::{debug, error};

use crate::config::ServerConfig;

/// Drive a single accepted TCP connection to completion.
///
/// Wraps `stream` in a Hyper HTTP/1.1 connection, dispatches every request
/// through [`service`], and logs errors.  The function is `async` and runs
/// inside a dedicated `tokio::spawn` task.
///
/// The connection holds a semaphore permit (passed by the accept loop via a
/// moved local variable `_permit`) that is released when this function returns.
pub async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: Arc<ServerConfig>,
) {
    debug!(peer = %peer_addr, "Connection accepted");

    // `TokioIo` bridges Tokio's `AsyncRead`/`AsyncWrite` to Hyper's `IO` trait.
    let io = TokioIo::new(stream);

    // Clone peer_addr into the service closure (cheap — it's Copy).
    let service = hyper::service::service_fn(move |req: Request<Incoming>| {
        let cfg = Arc::clone(&config);
        async move { Ok::<Response<Full<Bytes>>, Infallible>(route(req, peer_addr, &cfg)) }
    });

    match http1::Builder::new()
        .keep_alive(true)
        .serve_connection(io, service)
        .await
    {
        Ok(()) => debug!(peer = %peer_addr, "Connection closed cleanly"),
        Err(e) => {
            if e.is_incomplete_message() {
                // Client disconnected before finishing the request — normal for
                // health-check probes that close immediately after TCP connect.
                debug!(peer = %peer_addr, "Client disconnected mid-request");
            } else {
                error!(peer = %peer_addr, err = %e, "HTTP connection error");
            }
        }
    }
}

/// Dispatch an HTTP request and return a response.
///
/// In Phase 1 this is a minimal placeholder that always returns `200 OK`.
/// Phase 2 will replace this with the real router.
fn route(req: Request<Incoming>, _peer: SocketAddr, _cfg: &ServerConfig) -> Response<Full<Bytes>> {
    let path = req.uri().path();

    // Minimal routing: health-check endpoint + catch-all.
    match path {
        "/health" => build_response(StatusCode::OK, "ok\n"),
        _ => build_response(
            StatusCode::OK,
            "Hello from rust-highperf-server\n",
        ),
    }
}

/// Build a plain-text response with the given status and body.
fn build_response(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("server", "rust-highperf-server/0.1")
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .expect("response builder is infallible with valid inputs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Method;

    fn dummy_request(path: &str) -> Request<()> {
        Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(())
            .unwrap()
    }

    #[test]
    fn build_response_sets_status_and_headers() {
        let resp = build_response(StatusCode::OK, "hello\n");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            resp.headers().get("server").unwrap(),
            "rust-highperf-server/0.1"
        );
    }

    #[test]
    fn build_response_not_found() {
        let resp = build_response(StatusCode::NOT_FOUND, "not found\n");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn dummy_request_compiles() {
        // Ensure the helper compiles; actual routing tests are in integration tests.
        let req = dummy_request("/health");
        assert_eq!(req.uri().path(), "/health");
    }
}
