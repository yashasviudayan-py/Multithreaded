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
use std::sync::atomic::{AtomicU64, Ordering as AOrdering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use hyper::{Request, StatusCode};
use tower::{Layer, Service};
use tracing::debug;

use crate::http::response::{HttpResponse, ResponseBuilder};

/// How often (in `check()` calls) to scan for stale buckets.
const EVICT_EVERY_N_CHECKS: u64 = 10_000;
/// Remove buckets that have not been accessed for longer than this.
const STALE_BUCKET_TTL: Duration = Duration::from_secs(300); // 5 minutes

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
///
/// ## Memory management
/// Every [`EVICT_EVERY_N_CHECKS`] calls to [`check`][Self::check], entries
/// that have not been accessed for [`STALE_BUCKET_TTL`] are evicted to prevent
/// unbounded growth when client IPs rotate (NAT pools, cloud deployments).
pub struct RateLimiter {
    buckets: DashMap<IpAddr, TokenBucket>,
    rate_rps: u32,
    /// Monotonically-increasing call counter used to schedule eviction sweeps.
    check_count: AtomicU64,
}

impl RateLimiter {
    /// Create a new rate limiter allowing `rate_rps` requests/second per IP.
    pub fn new(rate_rps: u32) -> Self {
        Self {
            buckets: DashMap::new(),
            rate_rps,
            check_count: AtomicU64::new(0),
        }
    }

    /// Check and consume a token for `ip`.
    ///
    /// Returns `true` if the request is within the rate limit, `false` if it
    /// should be rejected.  Creates a fresh bucket on the first request from
    /// each IP.
    ///
    /// Periodically evicts IP buckets that have been idle for
    /// [`STALE_BUCKET_TTL`] to prevent unbounded [`DashMap`] growth.
    pub fn check(&self, ip: IpAddr) -> bool {
        let n = self.check_count.fetch_add(1, AOrdering::Relaxed);
        if n > 0 && n.is_multiple_of(EVICT_EVERY_N_CHECKS) {
            self.evict_stale();
        }

        self.buckets
            .entry(ip)
            .or_insert_with(|| TokenBucket::new(self.rate_rps))
            .try_consume()
    }

    /// Remove all buckets whose `last_refill` timestamp is older than
    /// [`STALE_BUCKET_TTL`].
    ///
    /// Called automatically by [`check`][Self::check] every
    /// [`EVICT_EVERY_N_CHECKS`] requests.  Uses [`DashMap::retain`] which
    /// holds individual shard locks — not a stop-the-world operation.
    fn evict_stale(&self) {
        // If the server (or system) has been running for less than STALE_BUCKET_TTL,
        // no bucket can possibly be stale yet — skip the sweep entirely.
        // The previous code fell back to `Instant::now()` as the cutoff, which
        // made every bucket appear stale and caused a full eviction on the very
        // first sweep during early uptime.
        let cutoff = match Instant::now().checked_sub(STALE_BUCKET_TTL) {
            Some(t) => t,
            None => return,
        };
        let before = self.buckets.len();
        self.buckets
            .retain(|_, bucket| bucket.last_refill >= cutoff);
        let removed = before.saturating_sub(self.buckets.len());
        if removed > 0 {
            debug!(removed, "Evicted stale rate-limiter buckets");
        }
    }

    /// Return the current number of tracked IP buckets (useful for monitoring).
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    /// Spawn a background Tokio task that runs [`evict_stale`] every 60 seconds.
    ///
    /// Complements the call-count-based sweep: on low-traffic servers the
    /// call-count trigger may fire infrequently, letting stale buckets
    /// accumulate.  The background task guarantees at most 60 s of extra
    /// memory per idle IP.
    ///
    /// The returned [`tokio::task::JoinHandle`] is intentionally dropped by
    /// the caller (`accept_loop`): the task runs until the process exits.
    pub fn start_eviction_task(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(60));
            // The first tick resolves immediately; subsequent ticks are 60 s apart.
            interval.tick().await; // skip the immediate first tick
            loop {
                interval.tick().await;
                self.evict_stale();
            }
        });
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
