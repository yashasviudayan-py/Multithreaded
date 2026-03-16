//! Concurrency-limiter Tower middleware.
//!
//! [`ConcurrencyLimiterLayer`] wraps an inner service and enforces a
//! server-wide cap on the number of requests being processed concurrently.
//! When the cap is reached every additional request receives an immediate
//! `503 Service Unavailable` response — no queuing, no blocking.
//!
//! Internally a [`Semaphore`] permit is acquired (non-blocking) at the start
//! of each request and released (via [`Drop`]) when the response future
//! completes, whether it succeeds, errors, or panics.
//!
//! # Usage
//! ```rust,ignore
//! use std::sync::Arc;
//! use tokio::sync::Semaphore;
//! use tower::ServiceBuilder;
//! use crate::middleware::concurrency::ConcurrencyLimiterLayer;
//!
//! let sem = Arc::new(Semaphore::new(5000));
//! let svc = ServiceBuilder::new()
//!     .layer(ConcurrencyLimiterLayer::new(Arc::clone(&sem)))
//!     .service(inner);
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper::{Request, StatusCode};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::{Layer, Service};
use tracing::debug;

use crate::http::response::{HttpResponse, ResponseBuilder};

// ── Layer ─────────────────────────────────────────────────────────────────────

/// Tower [`Layer`] that caps concurrent request processing server-wide.
pub struct ConcurrencyLimiterLayer {
    semaphore: Arc<Semaphore>,
}

impl ConcurrencyLimiterLayer {
    /// Create a layer backed by `semaphore`.
    ///
    /// The semaphore's initial permits equal the maximum allowed concurrency.
    /// Share the same [`Arc<Semaphore>`] across all connections created from
    /// the same server accept loop so the limit is truly server-wide.
    pub fn new(semaphore: Arc<Semaphore>) -> Self {
        Self { semaphore }
    }
}

impl<S> Layer<S> for ConcurrencyLimiterLayer {
    type Service = ConcurrencyLimiterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ConcurrencyLimiterService {
            inner,
            semaphore: Arc::clone(&self.semaphore),
        }
    }
}

// ── Service ───────────────────────────────────────────────────────────────────

/// Tower [`Service`] produced by [`ConcurrencyLimiterLayer`].
///
/// Attempts a non-blocking semaphore acquire on each request.  If no permit
/// is available a `503` is returned immediately.  The acquired permit is held
/// for the entire duration of the inner service future.
#[derive(Clone)]
pub struct ConcurrencyLimiterService<S> {
    inner: S,
    semaphore: Arc<Semaphore>,
}

impl<S, ReqBody> Service<Request<ReqBody>> for ConcurrencyLimiterService<S>
where
    S: Service<Request<ReqBody>, Response = HttpResponse, Error = Infallible> + Clone + Send + 'static,
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
        match Arc::clone(&self.semaphore).try_acquire_owned() {
            Ok(permit) => {
                let fut = self.inner.call(req);
                Box::pin(async move {
                    // Permit is held until the future completes (Drop).
                    let _permit: OwnedSemaphorePermit = permit;
                    fut.await
                })
            }
            Err(_) => {
                debug!("Concurrency limit reached — returning 503");
                Box::pin(std::future::ready(Ok(
                    ResponseBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
                        .header("retry-after", "1")
                        .text("503 Service Unavailable\n"),
                )))
            }
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::response::ResponseBuilder;
    use bytes::Bytes;
    use hyper::{Method, StatusCode};
    use tower::{Service, ServiceBuilder};

    fn make_req(method: Method, uri: &str) -> Request<Bytes> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Bytes::new())
            .unwrap()
    }

    fn ok_service() -> impl Service<
        Request<Bytes>,
        Response = HttpResponse,
        Error = Infallible,
        Future = std::future::Ready<Result<HttpResponse, Infallible>>,
    > + Clone {
        tower::service_fn(|_req: Request<Bytes>| {
            std::future::ready(Ok(ResponseBuilder::ok().text("ok\n")))
        })
    }

    #[tokio::test]
    async fn allows_request_when_permit_available() {
        let sem = Arc::new(Semaphore::new(2));
        let mut svc = ServiceBuilder::new()
            .layer(ConcurrencyLimiterLayer::new(Arc::clone(&sem)))
            .service(ok_service());

        let resp = svc.call(make_req(Method::GET, "/")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(sem.available_permits(), 2); // permit released after response
    }

    #[tokio::test]
    async fn returns_503_when_no_permits() {
        let sem = Arc::new(Semaphore::new(1));
        // Consume the only permit manually so the service sees an empty semaphore.
        let _guard = Arc::clone(&sem).try_acquire_owned().unwrap();

        let mut svc = ServiceBuilder::new()
            .layer(ConcurrencyLimiterLayer::new(Arc::clone(&sem)))
            .service(ok_service());

        let resp = svc.call(make_req(Method::GET, "/")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers().get("retry-after").unwrap(), "1");
    }

    #[tokio::test]
    async fn permit_released_after_response() {
        let sem = Arc::new(Semaphore::new(1));
        let mut svc = ServiceBuilder::new()
            .layer(ConcurrencyLimiterLayer::new(Arc::clone(&sem)))
            .service(ok_service());

        // First request: acquires and releases permit.
        let r1 = svc.call(make_req(Method::GET, "/")).await.unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        assert_eq!(sem.available_permits(), 1);

        // Second request: permit is available again.
        let r2 = svc.call(make_req(Method::GET, "/")).await.unwrap();
        assert_eq!(r2.status(), StatusCode::OK);
    }
}
