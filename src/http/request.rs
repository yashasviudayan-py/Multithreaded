//! HTTP request parsing with zero-copy support via httparse.
//!
//! [`HttpRequest`] wraps Hyper's parsed request parts plus the already-collected
//! request body, exposing ergonomic accessors for method, path, query parameters,
//! headers, and path parameters (populated by the router during dispatch).

use std::collections::HashMap;

use bytes::Bytes;
use hyper::http::request::Parts;
use hyper::{HeaderMap, Method, Uri};

/// A parsed, fully-buffered HTTP request.
///
/// Created by the connection handler after collecting the incoming body into a
/// [`Bytes`] buffer.  The router fills [`path_params`] before invoking the
/// matched handler.
#[derive(Debug)]
pub struct HttpRequest {
    /// HTTP method (GET, POST, PUT, DELETE, …).
    pub method: Method,
    /// Full request URI, including path and optional query string.
    pub uri: Uri,
    /// Parsed request headers.
    pub headers: HeaderMap,
    /// Fully collected request body.  Empty for methods without a body.
    pub body: Bytes,
    /// Path parameters extracted by the router (e.g. `{"id" => "42"}`).
    /// Empty until the router calls [`dispatch`][crate::http::router::Router::dispatch].
    pub path_params: HashMap<String, String>,
}

impl HttpRequest {
    /// Construct an [`HttpRequest`] from Hyper's split request parts and a
    /// pre-collected body.
    pub fn from_parts(parts: Parts, body: Bytes) -> Self {
        Self {
            method: parts.method,
            uri: parts.uri,
            headers: parts.headers,
            body,
            path_params: HashMap::new(),
        }
    }

    /// Return the request path without the query string (e.g. `"/users/42"`).
    pub fn path(&self) -> &str {
        self.uri.path()
    }

    /// Return the raw query string if present (e.g. `"foo=bar&baz=1"`).
    pub fn query(&self) -> Option<&str> {
        self.uri.query()
    }

    /// Look up a single query parameter by name.
    ///
    /// Performs a linear scan of the `key=value` pairs in the query string.
    /// Both the key comparison and the returned value are percent-decoded
    /// (`%XX` → byte, `+` → space) and interpreted as UTF-8.
    /// Returns `None` if the key is absent or if there is no query string.
    pub fn query_param(&self, key: &str) -> Option<String> {
        self.uri.query()?.split('&').find_map(|pair| {
            let mut kv = pair.splitn(2, '=');
            let k = kv.next()?;
            if percent_decode(k) == key {
                Some(percent_decode(kv.next().unwrap_or("")))
            } else {
                None
            }
        })
    }

    /// Return a path parameter captured by the router.
    ///
    /// For example, with the pattern `/users/:id` and the path `/users/42`,
    /// `path_param("id")` returns `Some("42")`.
    pub fn path_param(&self, key: &str) -> Option<&str> {
        self.path_params.get(key).map(String::as_str)
    }

    /// Return the string value of a named request header.
    ///
    /// Returns `None` if the header is absent or its value contains non-UTF-8
    /// bytes.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// Percent-decode a URL-encoded string (`%XX` sequences and `+` as space).
///
/// Decodes valid `%HH` hex sequences into their byte values and then
/// interprets the result as UTF-8.  Invalid sequences (non-hex digits, or
/// decoded bytes that form invalid UTF-8) are passed through unchanged.
fn percent_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| {
        // Decoded bytes are not valid UTF-8 (e.g. lone high bytes) — return
        // the original undecoded string rather than panic or corrupt data.
        String::from_utf8_lossy(e.as_bytes()).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(method: Method, uri: &str, body: &[u8]) -> HttpRequest {
        let (parts, _) = hyper::Request::builder()
            .method(method.clone())
            .uri(uri)
            .body(())
            .unwrap_or_else(|_| panic!("invalid test URI: {uri}"))
            .into_parts();
        HttpRequest::from_parts(parts, Bytes::copy_from_slice(body))
    }

    #[test]
    fn path_excludes_query_string() {
        let req = make_request(Method::GET, "/users/42?foo=bar", b"");
        assert_eq!(req.path(), "/users/42");
    }

    #[test]
    fn query_returns_raw_string() {
        let req = make_request(Method::GET, "/path?a=1&b=2", b"");
        assert_eq!(req.query(), Some("a=1&b=2"));
    }

    #[test]
    fn query_param_found() {
        let req = make_request(Method::GET, "/path?name=rust&version=2021", b"");
        assert_eq!(req.query_param("name").as_deref(), Some("rust"));
        assert_eq!(req.query_param("version").as_deref(), Some("2021"));
    }

    #[test]
    fn query_param_missing_key() {
        let req = make_request(Method::GET, "/path?a=1", b"");
        assert!(req.query_param("missing").is_none());
    }

    #[test]
    fn query_param_no_query_string() {
        let req = make_request(Method::GET, "/path", b"");
        assert!(req.query_param("any").is_none());
    }

    #[test]
    fn path_param_set_and_retrieved() {
        let mut req = make_request(Method::GET, "/users/99", b"");
        req.path_params.insert("id".to_string(), "99".to_string());
        assert_eq!(req.path_param("id"), Some("99"));
        assert_eq!(req.path_param("missing"), None);
    }

    #[test]
    fn body_bytes_stored() {
        let req = make_request(Method::POST, "/data", b"hello world");
        assert_eq!(req.body.as_ref(), b"hello world");
    }

    #[test]
    fn query_param_percent_decoded() {
        let req = make_request(Method::GET, "/search?q=hello%20world&lang=en", b"");
        assert_eq!(req.query_param("q").as_deref(), Some("hello world"));
    }

    #[test]
    fn query_param_plus_decoded_as_space() {
        let req = make_request(Method::GET, "/path?msg=hello+world", b"");
        assert_eq!(req.query_param("msg").as_deref(), Some("hello world"));
    }

    #[test]
    fn percent_decode_roundtrip() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("foo%3Dbar"), "foo=bar");
        assert_eq!(percent_decode("a+b+c"), "a b c");
        assert_eq!(percent_decode("no_encoding"), "no_encoding");
        assert_eq!(percent_decode("%"), "%"); // lone % passthrough
        assert_eq!(percent_decode("%GG"), "%GG"); // non-hex passthrough
    }

    #[test]
    fn header_accessor() {
        let (parts, _) = hyper::Request::builder()
            .method(Method::GET)
            .uri("/")
            .header("x-custom", "value123")
            .body(())
            .unwrap()
            .into_parts();
        let req = HttpRequest::from_parts(parts, Bytes::new());
        assert_eq!(req.header("x-custom"), Some("value123"));
        assert_eq!(req.header("x-missing"), None);
    }
}
