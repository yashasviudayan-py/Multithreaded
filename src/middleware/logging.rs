//! Structured request/response logging middleware via `tracing`.
//!
//! Wraps any inner [`Service`] and emits one structured log line per request
//! containing the peer address, HTTP method, path, response status, and wall-
//! clock latency.  Compatible with `tracing-subscriber`'s JSON formatter.
//!
//! # Usage
//! ```rust,ignore
//! use tower::ServiceBuilder;
//! use crate::middleware::LoggingLayer;
//!
//! let svc = ServiceBuilder::new()
//!     .layer(LoggingLayer::new(peer_addr))
//!     .service(inner);
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use hyper::Request;
use tower::{Layer, Service};
use tracing::info;

use crate::http::response::HttpResponse;

/// Tower [`Layer`] that wraps an inner service with per-request logging.
pub struct LoggingLayer {
    peer: SocketAddr,
}

impl LoggingLayer {
    /// Create a logging layer for connections from `peer`.
    pub fn new(peer: SocketAddr) -> Self {
        Self { peer }
    }
}

impl<S> Layer<S> for LoggingLayer {
    type Service = LoggingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LoggingService {
            inner,
            peer: self.peer,
        }
    }
}

/// Tower [`Service`] produced by [`LoggingLayer`].
///
/// Logs `method`, `path`, `status`, and `latency_ms` for every request/response
/// pair using structured [`tracing`] fields.
#[derive(Clone)]
pub struct LoggingService<S> {
    inner: S,
    peer: SocketAddr,
}

impl<S, ReqBody> Service<Request<ReqBody>> for LoggingService<S>
where
    S: Service<Request<ReqBody>, Response = HttpResponse, Error = Infallible>,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = HttpResponse;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let start = Instant::now();
        let method = req.method().to_string();
        let path = req.uri().path().to_string();
        let peer = self.peer;

        let fut = self.inner.call(req);

        Box::pin(async move {
            let result = fut.await;
            let elapsed_ms = start.elapsed().as_millis();
            let status = match &result {
                Ok(resp) => resp.status().as_u16(),
                Err(never) => match *never {},
            };
            info!(
                peer = %peer,
                method = %method,
                path = %path,
                status = status,
                latency_ms = elapsed_ms,
                "request"
            );
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::response::ResponseBuilder;
    use bytes::Bytes;
    use hyper::{Method, StatusCode};
    use tower::ServiceBuilder;

    // A minimal inner service that echoes back a fixed 200 OK.
    fn echo_service() -> impl Service<
        Request<Bytes>,
        Response = HttpResponse,
        Error = Infallible,
        Future = std::future::Ready<Result<HttpResponse, Infallible>>,
    > {
        tower::service_fn(|_req: Request<Bytes>| {
            std::future::ready(Ok(ResponseBuilder::ok().text("ok\n")))
        })
    }

    fn make_req(method: Method, uri: &str) -> Request<Bytes> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Bytes::new())
            .unwrap()
    }

    #[tokio::test]
    async fn logging_passes_response_through() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let mut svc = ServiceBuilder::new()
            .layer(LoggingLayer::new(peer))
            .service(echo_service());

        let resp = svc.call(make_req(Method::GET, "/")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn logging_does_not_alter_response_headers() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let mut svc = ServiceBuilder::new()
            .layer(LoggingLayer::new(peer))
            .service(echo_service());

        let resp = svc.call(make_req(Method::GET, "/health")).await.unwrap();
        // Inner service sets the server header; logging layer must not strip it.
        assert!(resp.headers().get("server").is_some());
    }
}
