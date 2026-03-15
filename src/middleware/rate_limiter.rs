//! Token-bucket rate limiter middleware (per client IP).
//!
//! [`RateLimiter`] tracks one [`TokenBucket`] per client IP address using a
//! lock-free [`DashMap`].  Each bucket refills at `rate_rps` tokens per second
//! up to a maximum of `rate_rps` tokens (burst = 1 second of capacity).
//!
//! The Tower [`RateLimiterLayer`] + [`RateLimiterService`] integrate this into
//! the middleware stack.  Requests that exceed the limit receive an immediate
//! `429 Too Many Requests` response with a `retry-after: 1` hint.
//!
//! # Usage
//! ```rust,ignore
//! use std::sync::Arc;
//! use tower::ServiceBuilder;
//! use crate::middleware::rate_limiter::{RateLimiter, RateLimiterLayer};
//!
//! let limiter = Arc::new(RateLimiter::new(100)); // 100 req/s per IP
//! let svc = ServiceBuilder::new()
//!     .layer(RateLimiterLayer::new(Arc::clone(&limiter), client_ip))
//!     .service(inner);
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use dashmap::DashMap;
use hyper::{Request, StatusCode};
use tower::{Layer, Service};
use tracing::debug;

use crate::http::response::{HttpResponse, ResponseBuilder};

// ── Token bucket ─────────────────────────────────────────────────────────────

/// A single token-bucket rate-limiter slot for one client IP.
///
/// Tokens are refilled continuously based on elapsed wall-clock time, up to
/// `capacity`.  Each allowed request consumes one token.
struct TokenBucket {
    /// Available tokens (fractional to allow smooth refill).
    tokens: f64,
    /// Maximum token count (= `rate_rps` at construction time).
    capacity: f64,
    /// Timestamp of the last refill operation.
    last_refill: Instant,
    /// Token refill rate in tokens/second.
    rate: f64,
}

impl TokenBucket {
    fn new(rate_rps: u32) -> Self {
        let cap = f64::from(rate_rps);
        Self {
            tokens: cap,
            capacity: cap,
            last_refill: Instant::now(),
            rate: cap,
        }
    }

    /// Refill tokens based on elapsed time, then attempt to consume one.
    ///
    /// Returns `true` if the request is allowed; `false` if the bucket is empty.
    fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

// ── Shared limiter state ──────────────────────────────────────────────────────

/// Shared token-bucket state: one bucket per client [`IpAddr`].
///
/// Uses [`DashMap`] for lock-free concurrent access across all connections.
/// Create once at server startup and share via [`Arc`].
pub struct RateLimiter {
    buckets: DashMap<IpAddr, TokenBucket>,
    rate_rps: u32,
}

impl RateLimiter {
    /// Create a new rate limiter allowing `rate_rps` requests/second per IP.
    pub fn new(rate_rps: u32) -> Self {
        Self {
            buckets: DashMap::new(),
            rate_rps,
        }
    }

    /// Check and consume a token for `ip`.
    ///
    /// Returns `true` if the request is within the rate limit, `false` if it
    /// should be rejected.  Creates a fresh bucket on the first request from
    /// each IP.
    pub fn check(&self, ip: IpAddr) -> bool {
        self.buckets
            .entry(ip)
            .or_insert_with(|| TokenBucket::new(self.rate_rps))
            .try_consume()
    }
}

// ── Tower Layer / Service ─────────────────────────────────────────────────────

/// Tower [`Layer`] that applies per-IP rate limiting to an inner service.
pub struct RateLimiterLayer {
    limiter: Arc<RateLimiter>,
    ip: IpAddr,
}

impl RateLimiterLayer {
    /// Create a layer for connections from `ip`, using the shared `limiter`.
    pub fn new(limiter: Arc<RateLimiter>, ip: IpAddr) -> Self {
        Self { limiter, ip }
    }
}

impl<S> Layer<S> for RateLimiterLayer {
    type Service = RateLimiterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimiterService {
            inner,
            limiter: Arc::clone(&self.limiter),
            ip: self.ip,
        }
    }
}

/// Tower [`Service`] produced by [`RateLimiterLayer`].
///
/// Each `call` checks the token bucket for `ip`.  If a token is available the
/// request is forwarded to `inner`; otherwise a `429` is returned immediately.
#[derive(Clone)]
pub struct RateLimiterService<S> {
    inner: S,
    limiter: Arc<RateLimiter>,
    ip: IpAddr,
}

impl<S, ReqBody> Service<Request<ReqBody>> for RateLimiterService<S>
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
        if !self.limiter.check(self.ip) {
            debug!(ip = %self.ip, "Rate limit exceeded — returning 429");
            // Return 429 without forwarding to inner.
            //
            // Note: `poll_ready` on `inner` was already called by the
            // middleware chain.  For our stateless `service_fn` inner this is
            // safe; a stateful inner service would need a different approach
            // (e.g., storing readiness state from poll_ready).
            return Box::pin(std::future::ready(Ok(ResponseBuilder::new(
                StatusCode::TOO_MANY_REQUESTS,
            )
            .header("retry-after", "1")
            .text("429 Too Many Requests\n"))));
        }

        Box::pin(self.inner.call(req))
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
    > {
        tower::service_fn(|_req: Request<Bytes>| {
            std::future::ready(Ok(ResponseBuilder::ok().text("ok\n")))
        })
    }

    #[test]
    fn token_bucket_allows_up_to_capacity() {
        let mut bucket = TokenBucket::new(3);
        assert!(bucket.try_consume()); // 1
        assert!(bucket.try_consume()); // 2
        assert!(bucket.try_consume()); // 3
        assert!(!bucket.try_consume()); // 4 — empty
    }

    #[test]
    fn rate_limiter_check_allows_then_rejects() {
        let limiter = RateLimiter::new(2);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(limiter.check(ip)); // token 1
        assert!(limiter.check(ip)); // token 2
        assert!(!limiter.check(ip)); // empty
    }

    #[test]
    fn different_ips_have_independent_buckets() {
        let limiter = RateLimiter::new(1);
        let ip1: IpAddr = "10.0.0.1".parse().unwrap();
        let ip2: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(limiter.check(ip1));
        assert!(!limiter.check(ip1)); // ip1 exhausted
        assert!(limiter.check(ip2)); // ip2 still has token
    }

    #[tokio::test]
    async fn service_passes_request_under_limit() {
        let limiter = Arc::new(RateLimiter::new(10));
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let mut svc = ServiceBuilder::new()
            .layer(RateLimiterLayer::new(Arc::clone(&limiter), ip))
            .service(ok_service());

        let resp = svc.call(make_req(Method::GET, "/")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn service_returns_429_when_limit_exceeded() {
        let limiter = Arc::new(RateLimiter::new(1));
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let mut svc = ServiceBuilder::new()
            .layer(RateLimiterLayer::new(Arc::clone(&limiter), ip))
            .service(ok_service());

        // First request: consumes the single token.
        let r1 = svc.call(make_req(Method::GET, "/")).await.unwrap();
        assert_eq!(r1.status(), StatusCode::OK);

        // Second request: bucket is empty — should get 429.
        let r2 = svc.call(make_req(Method::GET, "/")).await.unwrap();
        assert_eq!(r2.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r2.headers().get("retry-after").unwrap(), "1");
    }
}
