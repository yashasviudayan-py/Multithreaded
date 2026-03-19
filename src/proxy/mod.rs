//! HTTP reverse proxy support.
//!
//! [`proxy_request`] forwards an incoming [`HttpRequest`] to a configured
//! upstream base URL, strips hop-by-hop headers, and returns the upstream
//! response (status + headers + body) to the caller.
//!
//! # Configuration
//! Set the `PROXY_UPSTREAM` environment variable to enable reverse-proxy mode:
//!
//! ```text
//! PROXY_UPSTREAM=http://backend:3000
//! PROXY_STRIP_PREFIX=/api   # optional: strip this prefix from the forwarded path
//! ```
//!
//! All requests that do not match a local route are forwarded to the upstream.
//! Local routes always take priority over the catch-all proxy route.

use bytes::Bytes;
use hyper::{Response, StatusCode};
use tracing::error;

use crate::http::request::HttpRequest;
use crate::http::response::{full_body, HttpResponse, SERVER_HEADER};

/// Hop-by-hop headers that must NOT be forwarded between client and upstream
/// (RFC 7230 §6.1 "connection-specific" headers).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Forward `req` to `upstream_base`, optionally stripping `strip_prefix`
/// from the path before constructing the upstream URL.
///
/// All request headers except hop-by-hop and `host` are forwarded.
/// The upstream's status code and response headers (minus hop-by-hop) are
/// relayed to the caller verbatim.
///
/// Returns `502 Bad Gateway` on network or serialization failures.
pub async fn proxy_request(
    client: &reqwest::Client,
    req: &HttpRequest,
    upstream_base: &str,
    strip_prefix: Option<&str>,
) -> HttpResponse {
    // ── Build target URL ───────────────────────────────────────────────────
    let path = req.path();
    let effective_path = strip_prefix
        .and_then(|p| path.strip_prefix(p))
        .unwrap_or(path);
    let base = upstream_base.trim_end_matches('/');
    let target = match req.query() {
        Some(q) => format!("{base}{effective_path}?{q}"),
        None => format!("{base}{effective_path}"),
    };

    // ── Build reqwest request ──────────────────────────────────────────────
    let method = match reqwest::Method::from_bytes(req.method.as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => return bad_gateway("invalid HTTP method"),
    };

    let mut rb = client.request(method, &target);

    // Forward all request headers except hop-by-hop and `host`
    // (reqwest sets the correct Host based on the target URL).
    for (name, value) in &req.headers {
        let n = name.as_str();
        if n == "host" || HOP_BY_HOP.contains(&n) {
            continue;
        }
        rb = rb.header(n, value.as_bytes());
    }
    rb = rb.body(req.body.to_vec());

    // ── Send and translate upstream response ───────────────────────────────
    match rb.send().await {
        Ok(upstream) => {
            let status =
                StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let up_headers = upstream.headers().clone();
            let body_bytes: Bytes = match upstream.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    error!(err = %e, "Failed to read upstream response body");
                    return bad_gateway("upstream body read failed");
                }
            };

            let mut builder = Response::builder().status(status);

            // Forward upstream response headers (minus hop-by-hop).
            for (name, value) in &up_headers {
                if HOP_BY_HOP.contains(&name.as_str()) {
                    continue;
                }
                builder = builder.header(name.as_str(), value.as_bytes());
            }

            // Overwrite content-length (we buffer the body) and stamp our
            // server header.
            builder = builder
                .header("content-length", body_bytes.len().to_string())
                .header("server", SERVER_HEADER);

            builder.body(full_body(body_bytes)).unwrap_or_else(|e| {
                error!(err = %e, "Failed to assemble proxy response");
                bad_gateway("internal proxy error")
            })
        }
        Err(e) => {
            error!(err = %e, target = %target, "Proxy upstream unreachable");
            bad_gateway("upstream unreachable")
        }
    }
}

/// Construct a `502 Bad Gateway` response with a human-readable reason body.
fn bad_gateway(reason: &str) -> HttpResponse {
    let body = Bytes::from(format!("502 Bad Gateway: {reason}\n"));
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header("content-type", "text/plain; charset=utf-8")
        .header("content-length", body.len().to_string())
        .header("server", SERVER_HEADER)
        .body(full_body(body))
        .unwrap_or_else(|_| Response::new(full_body(Bytes::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_gateway_has_correct_status() {
        let resp = bad_gateway("test reason");
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn bad_gateway_body_contains_reason() {
        let resp = bad_gateway("some failure");
        let cl: usize = resp
            .headers()
            .get("content-length")
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(cl > 0, "content-length should be non-zero");
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
    }

    #[test]
    fn bad_gateway_has_server_header() {
        let resp = bad_gateway("x");
        assert_eq!(resp.headers().get("server").unwrap(), SERVER_HEADER);
    }
}
