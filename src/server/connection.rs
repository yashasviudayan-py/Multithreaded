//! Per-connection HTTP/1.1 handling via Hyper.
//!
//! Each accepted TCP stream is driven here inside its own `tokio::spawn` task.
//! The Hyper connection manages keep-alive, pipelining, and framing; this
//! module owns only the service function and error reporting.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tracing::{debug, error, warn};

use crate::config::ServerConfig;

/// Drive a single accepted TCP connection to completion.
///
/// Wraps `stream` in a Hyper HTTP/1.1 connection, dispatches every request
/// through the service function, and logs errors.  Runs inside a dedicated
/// `tokio::spawn` task; the semaphore permit is moved in by the caller.
pub async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: Arc<ServerConfig>,
) {
    debug!(peer = %peer_addr, "Connection accepted");

    // `TokioIo` bridges Tokio's `AsyncRead`/`AsyncWrite` to Hyper's `IO` trait.
    let io = TokioIo::new(stream);

    let service = hyper::service::service_fn(move |req: Request<Incoming>| {
        let cfg = Arc::clone(&config);
        async move {
            let (parts, body) = req.into_parts();

            // Drain the request body before routing.
            //
            // HTTP/1.1 keep-alive requires that the server either (a) fully
            // reads the request body or (b) closes the connection.  If we skip
            // this, any request with a body (POST, PUT, PATCH) will disable
            // keep-alive for that exchange, degrading throughput.
            //
            // We discard the collected bytes; the body is consumed only to
            // advance the TCP stream to the next request boundary.
            if let Err(e) = body.collect().await {
                // Client disconnected mid-upload — log and close.
                warn!(peer = %peer_addr, err = %e, "Failed to drain request body");
                return Ok::<Response<Full<Bytes>>, Infallible>(
                    build_error_response(StatusCode::BAD_REQUEST),
                );
            }

            let req = Request::from_parts(parts, ());
            Ok::<Response<Full<Bytes>>, Infallible>(route(req, peer_addr, &cfg))
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
/// In Phase 1 this is a minimal placeholder.  Phase 2 wires up the real router.
/// The body has already been consumed by the time this is called.
fn route(req: Request<()>, _peer: SocketAddr, _cfg: &ServerConfig) -> Response<Full<Bytes>> {
    match req.uri().path() {
        "/health" => build_response(StatusCode::OK, "ok\n"),
        _ => build_response(StatusCode::OK, "Hello from rust-highperf-server\n"),
    }
}

/// Build a plain-text `200 OK` (or other status) response with a static body.
fn build_response(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    // All inputs are compile-time constants: status is a valid HTTP status and
    // header values are ASCII strings, so the builder cannot fail here.
    match Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("server", "rust-highperf-server/0.1")
        .body(Full::new(Bytes::from_static(body.as_bytes())))
    {
        Ok(resp) => resp,
        Err(e) => {
            // Should be unreachable with static inputs, but avoid a panic.
            error!(err = %e, "Failed to build response — returning 500");
            build_error_response(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Build a minimal error response with an empty body.
fn build_error_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("server", "rust-highperf-server/0.1")
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
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
            .unwrap_or_else(|_| panic!("invalid test URI: {path}"))
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
    fn build_error_response_has_status() {
        let resp = build_error_response(StatusCode::BAD_REQUEST);
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn route_health_path() {
        let req = dummy_request("/health");
        let cfg = crate::config::ServerConfig::from_env().unwrap();
        let resp = route(req, "127.0.0.1:1234".parse().unwrap(), &cfg);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn route_unknown_path_returns_200() {
        let req = dummy_request("/anything");
        let cfg = crate::config::ServerConfig::from_env().unwrap();
        let resp = route(req, "127.0.0.1:1234".parse().unwrap(), &cfg);
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
