//! Route matching and handler dispatch.
//!
//! [`Router`] maps `(Method, path-pattern)` pairs to async handler functions.
//! Path patterns may contain named parameters prefixed with `:`, e.g.
//! `/users/:id/posts/:post_id`.  Parameters are extracted during dispatch and
//! made available on [`HttpRequest::path_param`].
//!
//! # Example
//! ```rust,ignore
//! let mut router = Router::new();
//! router.get("/", |_req| async { ResponseBuilder::ok().text("hello\n") });
//! router.get("/users/:id", |req| async move {
//!     let id = req.path_param("id").unwrap_or("unknown").to_string();
//!     ResponseBuilder::ok().text(format!("user {id}\n"))
//! });
//! let response = router.dispatch(request).await;
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hyper::Method;

use super::request::HttpRequest;
use super::response::{HttpResponse, ResponseBuilder};

/// A boxed, `Send`-able future resolving to an [`HttpResponse`].
pub type BoxFuture = Pin<Box<dyn Future<Output = HttpResponse> + Send>>;

/// Type-erased async handler function.
type HandlerFn = Arc<dyn Fn(HttpRequest) -> BoxFuture + Send + Sync>;

/// One segment of a compiled route pattern.
#[derive(Debug, Clone)]
enum PathSegment {
    /// A literal path component that must match exactly.
    Literal(String),
    /// A named capture (`:name`) that matches any single component.
    Param(String),
}

/// A registered route: method + compiled pattern + handler.
struct RouteEntry {
    method: Method,
    segments: Vec<PathSegment>,
    handler: HandlerFn,
}

/// HTTP router: maps `(Method, path)` pairs to async handlers.
///
/// Routes are matched in registration order.  The first route whose method
/// *and* path pattern both match the incoming request is dispatched.  If a
/// path matches but no registered method does, a `405 Method Not Allowed` is
/// returned.  Unmatched paths yield `404 Not Found`.
pub struct Router {
    routes: Vec<RouteEntry>,
}

impl Router {
    /// Create an empty router.
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    /// Register a handler for `GET pattern`.
    pub fn get<F, Fut>(&mut self, pattern: &str, handler: F) -> &mut Self
    where
        F: Fn(HttpRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HttpResponse> + Send + 'static,
    {
        self.add(Method::GET, pattern, handler)
    }

    /// Register a handler for `POST pattern`.
    pub fn post<F, Fut>(&mut self, pattern: &str, handler: F) -> &mut Self
    where
        F: Fn(HttpRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HttpResponse> + Send + 'static,
    {
        self.add(Method::POST, pattern, handler)
    }

    /// Register a handler for `PUT pattern`.
    pub fn put<F, Fut>(&mut self, pattern: &str, handler: F) -> &mut Self
    where
        F: Fn(HttpRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HttpResponse> + Send + 'static,
    {
        self.add(Method::PUT, pattern, handler)
    }

    /// Register a handler for `DELETE pattern`.
    pub fn delete<F, Fut>(&mut self, pattern: &str, handler: F) -> &mut Self
    where
        F: Fn(HttpRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HttpResponse> + Send + 'static,
    {
        self.add(Method::DELETE, pattern, handler)
    }

    /// Register a handler that matches *any* HTTP method for `pattern`.
    pub fn any<F, Fut>(&mut self, pattern: &str, handler: F) -> &mut Self
    where
        F: Fn(HttpRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HttpResponse> + Send + 'static,
    {
        // Use a sentinel method; `dispatch` checks for it explicitly.
        self.add(
            Method::from_bytes(b"*").expect("valid sentinel"),
            pattern,
            handler,
        )
    }

    /// Internal: erase the handler type and push the route.
    fn add<F, Fut>(&mut self, method: Method, pattern: &str, handler: F) -> &mut Self
    where
        F: Fn(HttpRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HttpResponse> + Send + 'static,
    {
        let segments = parse_pattern(pattern);
        let boxed: HandlerFn = Arc::new(move |req| Box::pin(handler(req)) as BoxFuture);
        self.routes.push(RouteEntry {
            method,
            segments,
            handler: boxed,
        });
        self
    }

    /// Dispatch `request` to the first matching handler.
    ///
    /// Matching rules:
    /// 1. For each route (in registration order), check if the path pattern
    ///    matches the request path.
    /// 2. If a path match is found, also check the method.
    /// 3. First method+path match wins.
    /// 4. If only path matched (no method), return `405 Method Not Allowed`.
    /// 5. If nothing matched, return `404 Not Found`.
    pub async fn dispatch(&self, request: HttpRequest) -> HttpResponse {
        let path = request.path().to_string();
        let method = request.method.clone();

        let mut any_path_matched = false;

        for route in &self.routes {
            if let Some(params) = route.match_path(&path) {
                any_path_matched = true;
                // Wildcard sentinel "*" matches every method.
                let method_ok = route.method.as_str() == "*" || route.method == method;
                if method_ok {
                    let mut req = request;
                    req.path_params = params;
                    return (route.handler)(req).await;
                }
            }
        }

        if any_path_matched {
            ResponseBuilder::method_not_allowed().empty()
        } else {
            ResponseBuilder::not_found().text("404 Not Found\n")
        }
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

// ── Path pattern helpers ──────────────────────────────────────────────────────

/// Parse a pattern string like `"/users/:id/posts"` into a vec of segments.
fn parse_pattern(pattern: &str) -> Vec<PathSegment> {
    pattern
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|seg| {
            if let Some(name) = seg.strip_prefix(':') {
                PathSegment::Param(name.to_string())
            } else {
                PathSegment::Literal(seg.to_string())
            }
        })
        .collect()
}

impl RouteEntry {
    /// Try to match `path` against this route's pattern.
    ///
    /// Returns `Some(params)` with captured values on success, or `None` if
    /// the path does not match.
    fn match_path(&self, path: &str) -> Option<HashMap<String, String>> {
        let path_segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        if path_segs.len() != self.segments.len() {
            return None;
        }

        let mut params = HashMap::new();
        for (pattern_seg, path_seg) in self.segments.iter().zip(path_segs.iter()) {
            match pattern_seg {
                PathSegment::Literal(lit) => {
                    if lit != path_seg {
                        return None;
                    }
                }
                PathSegment::Param(name) => {
                    params.insert(name.clone(), path_seg.to_string());
                }
            }
        }

        Some(params)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use hyper::StatusCode;

    /// Build a minimal `HttpRequest` for testing.
    fn make_req(method: Method, uri: &str) -> HttpRequest {
        let (parts, _) = hyper::Request::builder()
            .method(method)
            .uri(uri)
            .body(())
            .unwrap()
            .into_parts();
        HttpRequest::from_parts(parts, Bytes::new())
    }

    fn router_under_test() -> Router {
        let mut r = Router::new();
        r.get("/", |_req| async { ResponseBuilder::ok().text("root\n") });
        r.get("/health", |_req| async {
            ResponseBuilder::ok().text("ok\n")
        });
        r.get("/echo/:msg", |req| async move {
            let msg = req.path_param("msg").unwrap_or("").to_string();
            ResponseBuilder::ok().text(format!("{msg}\n"))
        });
        r.post("/items", |_req| async {
            ResponseBuilder::new(hyper::StatusCode::CREATED).text("created\n")
        });
        r
    }

    #[tokio::test]
    async fn root_path_dispatched() {
        let router = router_under_test();
        let resp = router.dispatch(make_req(Method::GET, "/")).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn literal_path_dispatched() {
        let router = router_under_test();
        let resp = router.dispatch(make_req(Method::GET, "/health")).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn path_param_extracted_and_echoed() {
        let router = router_under_test();
        let resp = router.dispatch(make_req(Method::GET, "/echo/hello")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), b"hello\n");
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let router = router_under_test();
        let resp = router.dispatch(make_req(Method::GET, "/not-real")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn wrong_method_returns_405() {
        let router = router_under_test();
        // /health is registered for GET only; POST should yield 405.
        let resp = router.dispatch(make_req(Method::POST, "/health")).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn post_route_dispatched() {
        let router = router_under_test();
        let resp = router.dispatch(make_req(Method::POST, "/items")).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn wildcard_method_matches_any() {
        let mut router = Router::new();
        router.any("/ping", |_req| async {
            ResponseBuilder::ok().text("pong\n")
        });
        for method in [Method::GET, Method::POST, Method::PUT, Method::DELETE] {
            let resp = router.dispatch(make_req(method, "/ping")).await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn multiple_path_params_extracted() {
        let mut router = Router::new();
        router.get("/users/:uid/posts/:pid", |req| async move {
            let uid = req.path_param("uid").unwrap_or("?").to_string();
            let pid = req.path_param("pid").unwrap_or("?").to_string();
            ResponseBuilder::ok().text(format!("{uid}/{pid}\n"))
        });
        let resp = router
            .dispatch(make_req(Method::GET, "/users/42/posts/7"))
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), b"42/7\n");
    }
}
