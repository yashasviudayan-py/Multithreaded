//! HTTP response building and serialization.
//!
//! [`ResponseBuilder`] provides a fluent API for constructing [`HttpResponse`]
//! values.  All responses automatically include the `server` header.
//!
//! The body type is [`HttpBody`] — a boxed, type-erased body that works for
//! both in-memory responses (via [`Full<Bytes>`]) and future streaming responses
//! (Phase 5+).  Use [`full_body`] to wrap a [`Bytes`] buffer.

use std::convert::Infallible;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::{Response, StatusCode};
use serde::Serialize;
use tracing::error;

/// Type-erased HTTP body.
///
/// Uses [`BoxBody<Bytes, Infallible>`] so in-memory and streaming bodies share
/// one concrete type.  The error type is [`Infallible`] because all I/O errors
/// must be handled *before* placing bytes in the body (e.g., file-read errors
/// become `404`/`500` responses rather than mid-body failures).
pub type HttpBody = BoxBody<Bytes, Infallible>;

/// A fully-constructed HTTP response ready to hand to Hyper.
pub type HttpResponse = Response<HttpBody>;

/// The value sent in the `server` response header on every response.
pub const SERVER_HEADER: &str = "rust-highperf-server/0.1";

/// Wrap a [`Bytes`] buffer in an [`HttpBody`].
///
/// This is the canonical way to create an owned, in-memory body for use with
/// [`ResponseBuilder`] or any code that constructs [`HttpResponse`] values
/// directly.
pub fn full_body(bytes: Bytes) -> HttpBody {
    Full::new(bytes)
        .map_err(|never: Infallible| match never {})
        .boxed()
}

/// Fluent builder for [`HttpResponse`] values.
///
/// # Example
/// ```rust,ignore
/// let resp = ResponseBuilder::ok().text("hello\n");
/// let resp = ResponseBuilder::not_found().text("not found\n");
/// let resp = ResponseBuilder::ok().json(&my_struct);
/// ```
pub struct ResponseBuilder {
    status: StatusCode,
    extra_headers: Vec<(&'static str, String)>,
}

impl ResponseBuilder {
    /// Create a builder with the given HTTP status code.
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            extra_headers: Vec::new(),
        }
    }

    /// `200 OK`.
    pub fn ok() -> Self {
        Self::new(StatusCode::OK)
    }

    /// `400 Bad Request`.
    pub fn bad_request() -> Self {
        Self::new(StatusCode::BAD_REQUEST)
    }

    /// `404 Not Found`.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND)
    }

    /// `405 Method Not Allowed`.
    pub fn method_not_allowed() -> Self {
        Self::new(StatusCode::METHOD_NOT_ALLOWED)
    }

    /// `500 Internal Server Error`.
    pub fn internal_error() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Append an extra response header.
    ///
    /// `key` must be a `&'static str` (compile-time constant) to avoid heap
    /// allocations for well-known header names.
    pub fn header(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.extra_headers.push((key, value.into()));
        self
    }

    /// Finish the response with a `text/plain; charset=utf-8` body.
    pub fn text(self, body: impl Into<String>) -> HttpResponse {
        let bytes = Bytes::from(body.into());
        self.build("text/plain; charset=utf-8", bytes)
    }

    /// Finish the response with an `application/json` body serialised from
    /// `value`.
    ///
    /// Falls back to a `500 Internal Server Error` response if serialisation
    /// fails (log entry emitted via `tracing`).
    pub fn json<T: Serialize>(self, value: &T) -> HttpResponse {
        match serde_json::to_vec(value) {
            Ok(vec) => self.build("application/json", Bytes::from(vec)),
            Err(e) => {
                error!(err = %e, "JSON serialisation failed; returning 500");
                ResponseBuilder::internal_error().text("Internal Server Error\n")
            }
        }
    }

    /// Finish the response with a raw bytes body and an explicit
    /// `content-type`.
    pub fn bytes_body(self, content_type: &'static str, body: Bytes) -> HttpResponse {
        self.build(content_type, body)
    }

    /// Finish the response with an empty body.
    pub fn empty(self) -> HttpResponse {
        self.build("text/plain; charset=utf-8", Bytes::new())
    }

    /// Assemble the final [`HttpResponse`].
    fn build(self, content_type: &'static str, body: Bytes) -> HttpResponse {
        let content_length = body.len().to_string();
        let mut builder = Response::builder()
            .status(self.status)
            .header("content-type", content_type)
            .header("content-length", content_length)
            .header("server", SERVER_HEADER);

        for (key, val) in self.extra_headers {
            builder = builder.header(key, val);
        }

        match builder.body(full_body(body)) {
            Ok(resp) => resp,
            Err(e) => {
                // Should be unreachable with the inputs we control.
                error!(err = %e, "ResponseBuilder::build failed — returning 500");
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header("server", SERVER_HEADER)
                    .body(full_body(Bytes::from_static(b"Internal Server Error\n")))
                    .unwrap_or_else(|_| Response::new(full_body(Bytes::new())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::collections::HashMap;

    async fn body_bytes(resp: HttpResponse) -> Bytes {
        resp.into_body().collect().await.unwrap().to_bytes()
    }

    #[tokio::test]
    async fn ok_text_sets_status_and_headers() {
        let resp = ResponseBuilder::ok().text("hello\n");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(resp.headers().get("server").unwrap(), SERVER_HEADER);
        assert_eq!(body_bytes(resp).await.as_ref(), b"hello\n");
    }

    #[tokio::test]
    async fn not_found_status() {
        let resp = ResponseBuilder::not_found().text("not found\n");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn method_not_allowed_status() {
        let resp = ResponseBuilder::method_not_allowed().empty();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn json_response() {
        let mut map = HashMap::new();
        map.insert("key", "value");
        let resp = ResponseBuilder::ok().json(&map);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        let bytes = body_bytes(resp).await;
        let parsed: HashMap<String, String> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[tokio::test]
    async fn extra_header_attached() {
        let resp = ResponseBuilder::ok()
            .header("x-request-id", "abc-123")
            .text("body");
        assert_eq!(resp.headers().get("x-request-id").unwrap(), "abc-123");
    }

    #[tokio::test]
    async fn content_length_matches_body() {
        let resp = ResponseBuilder::ok().text("hello");
        assert_eq!(resp.headers().get("content-length").unwrap(), "5");
    }

    #[tokio::test]
    async fn empty_body_has_zero_content_length() {
        let resp = ResponseBuilder::ok().empty();
        assert_eq!(resp.headers().get("content-length").unwrap(), "0");
        assert!(body_bytes(resp).await.is_empty());
    }

    #[tokio::test]
    async fn full_body_helper_roundtrips_bytes() {
        let original = Bytes::from_static(b"test data");
        let body = full_body(original.clone());
        let collected = body.collect().await.unwrap().to_bytes();
        assert_eq!(collected, original);
    }
}
