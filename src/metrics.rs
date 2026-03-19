//! Prometheus-compatible server metrics.
//!
//! [`Metrics`] holds a set of lock-free atomic counters that are incremented
//! throughout the request lifecycle.  The `/metrics` route renders them in
//! the [Prometheus text format v0.0.4] so they can be scraped by any
//! Prometheus-compatible monitoring system (Grafana, Victoria Metrics, …).
//!
//! [Prometheus text format v0.0.4]: https://prometheus.io/docs/instrumenting/exposition_formats/
//!
//! # Exposed metrics
//! | Metric name                       | Type    | Description                              |
//! |-----------------------------------|---------|------------------------------------------|
//! | `requests_total`                  | counter | Total HTTP requests received             |
//! | `requests_active`                 | gauge   | Requests currently in-flight             |
//! | `responses_2xx_total`             | counter | Responses with a 2xx status code         |
//! | `responses_4xx_total`             | counter | Responses with a 4xx status code         |
//! | `responses_5xx_total`             | counter | Responses with a 5xx status code         |
//! | `rate_limited_total`              | counter | Requests rejected with 429 Too Many Req. |
//! | `concurrency_limited_total`       | counter | Requests rejected with 503 (cap reached) |
//! | `timed_out_total`                 | counter | Requests that hit REQUEST_TIMEOUT_SECS   |
//! | `connections_accepted_total`      | counter | Total TCP connections accepted           |
//! | `connections_rejected_ip_total`   | counter | Connections dropped by IP filter         |

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Shared, lock-free metrics counters.
///
/// Construct once at server startup and share via `Arc`.  All operations use
/// relaxed ordering — these counters are best-effort monitoring data, not
/// synchronisation primitives.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Total HTTP requests received (incremented before dispatch).
    pub requests_total: AtomicU64,
    /// In-flight requests currently being processed.
    pub requests_active: AtomicU64,
    /// Responses with a 2xx status code.
    pub responses_2xx: AtomicU64,
    /// Responses with a 4xx status code.
    pub responses_4xx: AtomicU64,
    /// Responses with a 5xx status code.
    pub responses_5xx: AtomicU64,
    /// Requests rejected by the rate limiter (429).
    pub rate_limited: AtomicU64,
    /// Requests rejected by the concurrency limiter (503 at cap).
    pub concurrency_limited: AtomicU64,
    /// Requests that exceeded REQUEST_TIMEOUT_SECS.
    pub timed_out: AtomicU64,
    /// Total TCP connections accepted (before IP filter).
    pub connections_accepted: AtomicU64,
    /// Connections dropped by the IP filter (blocked or not allowlisted).
    pub connections_rejected_ip: AtomicU64,
}

impl Metrics {
    /// Create a new zeroed [`Metrics`] instance.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Render all metrics in Prometheus text format v0.0.4.
    ///
    /// Output is a plain-text string suitable for the `/metrics` endpoint.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(1024);

        fn gauge(out: &mut String, name: &str, help: &str, value: u64) {
            out.push_str(&format!("# HELP {name} {help}\n"));
            out.push_str(&format!("# TYPE {name} gauge\n"));
            out.push_str(&format!("{name} {value}\n"));
        }
        fn counter(out: &mut String, name: &str, help: &str, value: u64) {
            out.push_str(&format!("# HELP {name} {help}\n"));
            out.push_str(&format!("# TYPE {name} counter\n"));
            out.push_str(&format!("{name}_total {value}\n"));
        }

        counter(
            &mut out,
            "requests",
            "Total HTTP requests received.",
            self.requests_total.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "requests_active",
            "HTTP requests currently in-flight.",
            self.requests_active.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "responses_2xx",
            "Responses with a 2xx status code.",
            self.responses_2xx.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "responses_4xx",
            "Responses with a 4xx status code.",
            self.responses_4xx.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "responses_5xx",
            "Responses with a 5xx status code.",
            self.responses_5xx.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "rate_limited",
            "Requests rejected by the rate limiter (429).",
            self.rate_limited.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "concurrency_limited",
            "Requests rejected by the concurrency limiter (503).",
            self.concurrency_limited.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "timed_out",
            "Requests that exceeded the per-request processing timeout.",
            self.timed_out.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "connections_accepted",
            "Total TCP connections accepted.",
            self.connections_accepted.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "connections_rejected_ip",
            "Connections dropped by the IP filter.",
            self.connections_rejected_ip.load(Ordering::Relaxed),
        );

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_all_metric_names() {
        let m = Metrics::new();
        m.requests_total.store(42, Ordering::Relaxed);
        m.responses_2xx.store(40, Ordering::Relaxed);
        m.responses_4xx.store(1, Ordering::Relaxed);
        m.responses_5xx.store(1, Ordering::Relaxed);
        let text = m.render();
        assert!(text.contains("requests_total 42"));
        assert!(text.contains("responses_2xx_total 40"));
        assert!(text.contains("responses_4xx_total 1"));
        assert!(text.contains("responses_5xx_total 1"));
        assert!(text.contains("# TYPE requests counter"));
        assert!(text.contains("# TYPE requests_active gauge"));
    }

    #[test]
    fn render_starts_clean() {
        let m = Metrics::new();
        let text = m.render();
        assert!(text.contains("requests_total 0"));
        assert!(text.contains("requests_active 0"));
    }
}
