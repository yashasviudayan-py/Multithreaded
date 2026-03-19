//! Per-connection HTTP/1.1 and HTTP/2 handling via Hyper.
//!
//! Each accepted TCP stream is driven here inside its own `tokio::spawn` task.
//! Hyper manages keep-alive, pipelining, and framing; this module builds the
//! application [`Router`] and dispatches every request through it via a
//! composed Tower middleware stack:
//!
//! ```text
//! LoggingLayer          ←  outermost (measures full latency, logs status)
//!   └─ RateLimiterLayer        (per-IP token-bucket; returns 429 on exhaustion)
//!        └─ ConcurrencyLimiterLayer  (global cap; returns 503 when full)
//!             └─ service_fn          (collect body, enforce size limit, dispatch)
//! ```
//!
//! Protocol selection uses `hyper_util::server::conn::auto::Builder`, which
//! automatically negotiates HTTP/2 or HTTP/1.1 based on the ALPN extension
//! during the TLS handshake (or defaults to HTTP/1.1 for plain connections).
//!
//! ## Graceful shutdown
//! The accept loop passes a [`watch::Receiver<bool>`] that becomes `true` when
//! a shutdown signal is received.  `handle_connection` detects this inside its
//! `select!` loop and calls `Connection::graceful_shutdown`, which signals the
//! protocol layer to finish the current exchange and close the connection.
//!
//! ## Keep-alive timeout
//! A [`tokio::time::sleep`] timer is armed when the connection is established.
//! If it fires before the connection closes naturally the task exits, which
//! drops the TCP stream and lets the OS close the socket.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use sqlx::SqlitePool;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{watch, Semaphore};
use tower::ServiceBuilder;
use tracing::{debug, error, warn};

use crate::config::ServerConfig;
use crate::db;
use crate::http::request::HttpRequest;
use crate::http::response::{HttpResponse, ResponseBuilder};
use crate::http::router::Router;
use crate::metrics::Metrics;
use crate::middleware::{
    extract_bearer, ConcurrencyLimiterLayer, JwtSecret, LoggingLayer, RateLimiter, RateLimiterLayer,
};
use crate::server::task::run_blocking;
use crate::session::{extract_session_cookie, SessionStore};
use crate::static_files;
use crate::templates::TemplateEngine;

/// Shared application state threaded through every connection.
///
/// Bundles together the resources that are created once at server startup and
/// shared (via `Arc`) across all connection tasks.  Using a single struct keeps
/// the [`handle_connection`] signature within clippy's argument-count limit.
#[derive(Clone)]
pub struct AppState {
    /// SQLite connection pool.
    pub db_pool: Arc<SqlitePool>,
    /// JWT signing / verification keys.
    pub jwt: Arc<JwtSecret>,
    /// Prometheus-compatible server metrics counters.
    pub metrics: Arc<Metrics>,
    /// Cookie-based session store (server-side, UUID tokens).
    pub sessions: Arc<SessionStore>,
    /// Tera HTML template engine (`None` when `templates/` dir is missing).
    pub template_engine: Option<TemplateEngine>,
}

/// Drive a single accepted connection to completion.
///
/// Generic over `S` so the same logic handles both plain [`tokio::net::TcpStream`]
/// and TLS-wrapped streams (`tokio_rustls::server::TlsStream<TcpStream>`).
///
/// Builds the Tower middleware stack and drives the Hyper HTTP/1.1 or HTTP/2
/// connection state machine.  Protocol selection is automatic via
/// `auto::Builder` (ALPN for TLS; HTTP/1.1 for plain connections).
/// The [`watch::Receiver`] is used to signal graceful shutdown across all
/// active connections simultaneously.
pub async fn handle_connection<S>(
    stream: S,
    peer_addr: SocketAddr,
    config: Arc<ServerConfig>,
    rate_limiter: Arc<RateLimiter>,
    concurrency_limiter: Arc<Semaphore>,
    mut shutdown_rx: watch::Receiver<bool>,
    state: AppState,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    debug!(peer = %peer_addr, "Connection accepted");

    // Build the router once per connection; all keep-alive requests on this
    // connection share the same Arc<Router>.
    let router = Arc::new(build_router(
        &config,
        Arc::clone(&state.db_pool),
        Arc::clone(&state.jwt),
        Arc::clone(&state.metrics),
        Arc::clone(&state.sessions),
        state.template_engine.clone(),
    ));
    let max_body = config.max_body_bytes;
    let request_timeout = config.request_timeout_secs;
    let metrics = Arc::clone(&state.metrics);

    // ── Inner service: body collection + dispatch ──────────────────────────
    // Use tower::service_fn (not hyper::service::service_fn) so the result
    // implements tower::Service and can be composed with Tower middleware layers.
    let inner = tower::service_fn(move |req: Request<Incoming>| {
        let router = Arc::clone(&router);
        let m = Arc::clone(&metrics);
        async move {
            m.requests_total.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            m.requests_active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            let (parts, body) = req.into_parts();

            // Enforce body size limit before collecting.  Check Content-Length
            // for an early, cheap rejection of well-behaved clients.
            if let Some(cl) = parts
                .headers
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<usize>().ok())
            {
                if cl > max_body {
                    return Ok::<HttpResponse, Infallible>(
                        ResponseBuilder::new(StatusCode::PAYLOAD_TOO_LARGE)
                            .text("413 Payload Too Large\n"),
                    );
                }
            }

            // HTTP/1.1 keep-alive requires the server to fully consume the
            // request body before the next request on the same connection can
            // begin.  Collect and enforce the runtime limit.
            let body_bytes: Bytes = match body.collect().await {
                Ok(collected) => {
                    let bytes = collected.to_bytes();
                    if bytes.len() > max_body {
                        return Ok(ResponseBuilder::new(StatusCode::PAYLOAD_TOO_LARGE)
                            .text("413 Payload Too Large\n"));
                    }
                    bytes
                }
                Err(e) => {
                    warn!(peer = %peer_addr, err = %e, "Failed to collect request body");
                    return Ok(ResponseBuilder::bad_request().empty());
                }
            };

            let request = HttpRequest::from_parts(parts, body_bytes);
            // Enforce per-request processing timeout so slow handlers
            // cannot hold a worker indefinitely.
            let response = match tokio::time::timeout(
                Duration::from_secs(request_timeout),
                router.dispatch(request),
            )
            .await
            {
                Ok(resp) => resp,
                Err(_elapsed) => {
                    warn!(
                        peer = %peer_addr,
                        timeout_secs = request_timeout,
                        "Request processing timed out"
                    );
                    m.timed_out.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    ResponseBuilder::new(StatusCode::SERVICE_UNAVAILABLE)
                        .text("503 Service Unavailable: processing timeout\n")
                }
            };

            // Update status-class counters and decrement active gauge.
            let sc = response.status().as_u16();
            if sc < 300 {
                m.responses_2xx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else if sc < 500 {
                m.responses_4xx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                m.responses_5xx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            m.requests_active.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            Ok::<HttpResponse, Infallible>(response)
        }
    });

    // ── Compose Tower middleware stack ─────────────────────────────────────
    // Request flow: LoggingService → RateLimiterService → ConcurrencyLimiterService → inner
    let tower_stack = ServiceBuilder::new()
        .layer(LoggingLayer::new(peer_addr))
        .layer(RateLimiterLayer::new(rate_limiter, peer_addr.ip()))
        .layer(ConcurrencyLimiterLayer::new(concurrency_limiter))
        .service(inner);

    let service = TowerToHyperService::new(tower_stack);
    let io = TokioIo::new(stream);

    // ── Drive connection with keep-alive timeout + graceful shutdown ───────
    // `auto::Builder` handles HTTP/1.1 and HTTP/2 transparently: for TLS
    // connections it reads the ALPN extension negotiated during handshake; for
    // plain TCP it defaults to HTTP/1.1.
    let builder = auto::Builder::new(TokioExecutor::new());
    let conn = builder.serve_connection(io, service);
    tokio::pin!(conn);

    // Keep-alive idle timeout: drop the connection if it sits idle for too long.
    let ka_timeout = tokio::time::sleep(Duration::from_secs(config.keep_alive_timeout_secs));
    tokio::pin!(ka_timeout);

    // Track whether we have already initiated graceful shutdown on this
    // connection (prevents calling graceful_shutdown() more than once).
    let mut graceful = false;

    loop {
        tokio::select! {
            biased;

            result = &mut conn => {
                match result {
                    Ok(()) => debug!(peer = %peer_addr, "Connection closed cleanly"),
                    Err(e) => {
                        // auto::Builder's error type is Box<dyn Error>; downcast
                        // to hyper::Error to check for the benign "incomplete
                        // message" case (client closed mid-request).
                        let incomplete = e
                            .downcast_ref::<hyper::Error>()
                            .map(|he| he.is_incomplete_message())
                            .unwrap_or(false);
                        if incomplete {
                            debug!(peer = %peer_addr, "Client disconnected mid-request");
                        } else {
                            error!(peer = %peer_addr, err = %e, "HTTP connection error");
                        }
                    }
                }
                break;
            }

            // Listen for the server-wide graceful-shutdown broadcast.
            _ = shutdown_rx.changed(), if !graceful => {
                if *shutdown_rx.borrow() {
                    graceful = true;
                    debug!(peer = %peer_addr, "Graceful shutdown: sending Connection: close");
                    conn.as_mut().graceful_shutdown();
                }
                // If the value changed but is still false (shouldn't happen),
                // just continue polling.
            }

            // Keep-alive idle timeout: close the connection if unused too long.
            _ = &mut ka_timeout, if !graceful => {
                debug!(peer = %peer_addr, "Keep-alive timeout — closing idle connection");
                break;
            }
        }
    }
}

/// Build the application router.
///
/// Called once per accepted connection.  The router is wrapped in an `Arc` by
/// `handle_connection` so it is shared across all keep-alive requests on the
/// same connection without cloning.
///
/// `db` and `jwt` are shared via `Arc` clones captured in route closures.
pub(crate) fn build_router(
    cfg: &ServerConfig,
    db: Arc<SqlitePool>,
    jwt: Arc<JwtSecret>,
    metrics: Arc<Metrics>,
    sessions: Arc<SessionStore>,
    tmpl: Option<TemplateEngine>,
) -> Router {
    let mut router = Router::new();

    // Auth credentials from config (moved out of hard-coded strings).
    let cfg_auth_username = cfg.auth_username.clone();
    let cfg_auth_password = cfg.auth_password.clone();

    router.get("/", |_req| async {
        ResponseBuilder::ok().text("Hello from rust-highperf-server\n")
    });

    router.get("/health", |_req| async {
        ResponseBuilder::ok().text("ok\n")
    });

    // Example of path-parameter routing added in Phase 2.
    router.get("/echo/:message", |req| async move {
        let msg = req.path_param("message").unwrap_or("").to_string();
        ResponseBuilder::ok().text(format!("{msg}\n"))
    });

    // CPU-bound demo route added in Phase 3.
    router.get("/fib/:n", |req| async move {
        let n: u64 = req
            .path_param("n")
            .and_then(|v| v.parse().ok())
            .unwrap_or(10)
            .min(50);

        match run_blocking(move || fib(n)).await {
            Ok(result) => ResponseBuilder::ok().text(format!("{result}\n")),
            Err(e) => {
                tracing::error!(err = %e, "Blocking fib task failed");
                ResponseBuilder::internal_error().empty()
            }
        }
    });

    // Static file serving added in Phase 4.
    let static_dir = PathBuf::from(&cfg.static_dir);
    router.get("/static/*filepath", move |req| {
        let base = static_dir.clone();
        async move {
            let filepath = req.path_param("filepath").unwrap_or("").to_string();
            static_files::serve_file(&base, &filepath).await
        }
    });

    // ── Phase 8: JWT authentication + SQLite CRUD routes ──────────────────

    // POST /auth/token — issue a JWT for valid credentials.
    //
    // Accepts: `{"username": "...", "password": "..."}`
    // Returns: `{"token": "..."}`  or 401 on bad credentials.
    //
    // Credentials are loaded from AUTH_USERNAME / AUTH_PASSWORD environment
    // variables (see ServerConfig).  In production, replace with a database
    // lookup against hashed passwords.
    let jwt_for_token = Arc::clone(&jwt);
    router.post("/auth/token", move |req| {
        let jwt = Arc::clone(&jwt_for_token);
        let expected_user = cfg_auth_username.clone();
        let expected_pass = cfg_auth_password.clone();
        async move {
            #[derive(serde::Deserialize)]
            struct LoginPayload {
                username: String,
                password: String,
            }
            let payload: LoginPayload = match serde_json::from_slice(&req.body) {
                Ok(p) => p,
                Err(_) => {
                    return ResponseBuilder::new(StatusCode::BAD_REQUEST)
                        .json(&serde_json::json!({"error": "invalid JSON body"}))
                }
            };
            if payload.username != expected_user || payload.password != expected_pass {
                return ResponseBuilder::new(StatusCode::UNAUTHORIZED)
                    .json(&serde_json::json!({"error": "invalid credentials"}));
            }
            match jwt.create_token(&payload.username) {
                Ok(token) => ResponseBuilder::ok().json(&serde_json::json!({"token": token})),
                Err(e) => {
                    error!(err = %e, "Failed to create JWT");
                    ResponseBuilder::internal_error().empty()
                }
            }
        }
    });

    // GET /api/items — list all items (public).
    let db_list = Arc::clone(&db);
    router.get("/api/items", move |_req| {
        let pool = Arc::clone(&db_list);
        async move {
            match db::list_items(&pool).await {
                Ok(items) => ResponseBuilder::ok().json(&serde_json::json!(items)),
                Err(e) => {
                    error!(err = %e, "list_items failed");
                    ResponseBuilder::internal_error().empty()
                }
            }
        }
    });

    // GET /api/items/:id — fetch a single item by ID (public).
    let db_get = Arc::clone(&db);
    router.get("/api/items/:id", move |req| {
        let pool = Arc::clone(&db_get);
        async move {
            let id = req.path_param("id").unwrap_or("").to_string();
            match db::get_item(&pool, &id).await {
                Ok(Some(item)) => ResponseBuilder::ok().json(&serde_json::json!(item)),
                Ok(None) => ResponseBuilder::new(StatusCode::NOT_FOUND)
                    .json(&serde_json::json!({"error": "item not found"})),
                Err(e) => {
                    error!(err = %e, "get_item failed");
                    ResponseBuilder::internal_error().empty()
                }
            }
        }
    });

    // POST /api/admin/items — create an item (requires valid JWT).
    let db_create = Arc::clone(&db);
    let jwt_create = Arc::clone(&jwt);
    router.post("/api/admin/items", move |req| {
        let pool = Arc::clone(&db_create);
        let jwt = Arc::clone(&jwt_create);
        async move {
            // Validate JWT.
            let auth_header = req.header("authorization").map(str::to_owned);
            let token = match extract_bearer(auth_header.as_deref()) {
                Some(t) => t.to_owned(),
                None => {
                    return ResponseBuilder::new(StatusCode::UNAUTHORIZED)
                        .json(&serde_json::json!({"error": "missing Bearer token"}))
                }
            };
            if let Err(e) = jwt.verify_token(&token) {
                debug!(err = %e, "JWT verification failed");
                return ResponseBuilder::new(StatusCode::UNAUTHORIZED)
                    .json(&serde_json::json!({"error": "invalid or expired token"}));
            }
            // Parse body.
            let payload: db::CreateItem = match serde_json::from_slice(&req.body) {
                Ok(p) => p,
                Err(_) => {
                    return ResponseBuilder::new(StatusCode::BAD_REQUEST)
                        .json(&serde_json::json!({"error": "invalid JSON body"}))
                }
            };
            match db::create_item(&pool, &payload.name, &payload.description).await {
                Ok(item) => {
                    ResponseBuilder::new(StatusCode::CREATED).json(&serde_json::json!(item))
                }
                Err(e) => {
                    error!(err = %e, "create_item failed");
                    ResponseBuilder::internal_error().empty()
                }
            }
        }
    });

    // DELETE /api/admin/items/:id — delete an item (requires valid JWT).
    let db_delete = Arc::clone(&db);
    let jwt_delete = Arc::clone(&jwt);
    router.delete("/api/admin/items/:id", move |req| {
        let pool = Arc::clone(&db_delete);
        let jwt = Arc::clone(&jwt_delete);
        async move {
            // Validate JWT.
            let auth_header = req.header("authorization").map(str::to_owned);
            let token = match extract_bearer(auth_header.as_deref()) {
                Some(t) => t.to_owned(),
                None => {
                    return ResponseBuilder::new(StatusCode::UNAUTHORIZED)
                        .json(&serde_json::json!({"error": "missing Bearer token"}))
                }
            };
            if let Err(e) = jwt.verify_token(&token) {
                debug!(err = %e, "JWT verification failed");
                return ResponseBuilder::new(StatusCode::UNAUTHORIZED)
                    .json(&serde_json::json!({"error": "invalid or expired token"}));
            }
            let id = req.path_param("id").unwrap_or("").to_string();
            match db::delete_item(&pool, &id).await {
                Ok(true) => ResponseBuilder::ok().json(&serde_json::json!({"deleted": true})),
                Ok(false) => ResponseBuilder::new(StatusCode::NOT_FOUND)
                    .json(&serde_json::json!({"error": "item not found"})),
                Err(e) => {
                    error!(err = %e, "delete_item failed");
                    ResponseBuilder::internal_error().empty()
                }
            }
        }
    });

    // ── HTML UI routes (require TemplateEngine) ────────────────────────────
    // These routes are only registered when `templates/` is present at startup.
    // They provide a browser-friendly interface backed by the same SQLite DB.
    if let Some(ref engine) = tmpl {
        // Helper: extract current session user from Cookie header.
        // Shared across all /ui routes via Arc<SessionStore>.

        // GET /ui — redirect to /ui/index
        router.get("/ui", |_req| async {
            ResponseBuilder::new(hyper::StatusCode::FOUND)
                .header("location", "/ui/index")
                .empty()
        });

        // GET /ui/index — home page
        let e = engine.clone();
        let sess_home = Arc::clone(&sessions);
        router.get("/ui/index", move |req| {
            let engine = e.clone();
            let sessions = Arc::clone(&sess_home);
            async move {
                let token = req.header("cookie").and_then(|c| extract_session_cookie(Some(c)).map(str::to_owned));
                let session_user = token.as_deref().and_then(|t| sessions.get(t));
                let mut ctx = tera::Context::new();
                if let Some(ref u) = session_user { ctx.insert("session_user", u); }
                engine.render("index.html", &ctx)
            }
        });

        // GET /ui/login
        let e = engine.clone();
        router.get("/ui/login", move |_req| {
            let engine = e.clone();
            async move {
                let ctx = tera::Context::new();
                engine.render("login.html", &ctx)
            }
        });

        // POST /ui/login — verify credentials, set session cookie
        let e = engine.clone();
        let sess_login = Arc::clone(&sessions);
        let login_user = cfg.auth_username.clone();
        let login_pass = cfg.auth_password.clone();
        router.post("/ui/login", move |req| {
            let engine = e.clone();
            let sessions = Arc::clone(&sess_login);
            let expected_user = login_user.clone();
            let expected_pass = login_pass.clone();
            async move {
                // Parse application/x-www-form-urlencoded body.
                let body_str = String::from_utf8_lossy(&req.body).to_string();
                let mut username = String::new();
                let mut password = String::new();
                for pair in body_str.split('&') {
                    if let Some((k, v)) = pair.split_once('=') {
                        let val = v.replace('+', " ");
                        let decoded = percent_encoding::percent_decode_str(&val)
                            .decode_utf8_lossy()
                            .to_string();
                        if k == "username" { username = decoded.clone(); }
                        if k == "password" { password = decoded; }
                    }
                }
                if username == expected_user && password == expected_pass {
                    let token = sessions.create(username.clone());
                    let cookie = format!(
                        "session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=3600"
                    );
                    ResponseBuilder::new(hyper::StatusCode::FOUND)
                        .header("location", "/ui/items")
                        .header("set-cookie", &cookie)
                        .empty()
                } else {
                    let mut ctx = tera::Context::new();
                    ctx.insert("flash", "Invalid username or password.");
                    ctx.insert("flash_type", "error");
                    engine.render("login.html", &ctx)
                }
            }
        });

        // GET /ui/logout — destroy session
        let sess_logout = Arc::clone(&sessions);
        router.get("/ui/logout", move |req| {
            let sessions = Arc::clone(&sess_logout);
            async move {
                let token = req.header("cookie")
                    .and_then(|c| extract_session_cookie(Some(c)).map(str::to_owned));
                if let Some(t) = token.as_deref() {
                    sessions.remove(t);
                }
                ResponseBuilder::new(hyper::StatusCode::FOUND)
                    .header("location", "/ui/login")
                    .header("set-cookie", "session=; Max-Age=0; Path=/")
                    .empty()
            }
        });

        // GET /ui/items — list items (HTML)
        let e = engine.clone();
        let db_ui_list = Arc::clone(&db);
        let sess_items = Arc::clone(&sessions);
        router.get("/ui/items", move |req| {
            let engine = e.clone();
            let pool = Arc::clone(&db_ui_list);
            let sessions = Arc::clone(&sess_items);
            async move {
                let token = req.header("cookie")
                    .and_then(|c| extract_session_cookie(Some(c)).map(str::to_owned));
                let session_user = token.as_deref().and_then(|t| sessions.get(t));
                let items = db::list_items(&pool).await.unwrap_or_default();
                let mut ctx = tera::Context::new();
                if let Some(ref u) = session_user { ctx.insert("session_user", u); }
                ctx.insert("items", &items);
                // Pass any flash message from query string.
                if let Some(msg) = req.query_param("flash") {
                    ctx.insert("flash", &msg);
                }
                engine.render("items.html", &ctx)
            }
        });

        // POST /ui/items — create item (requires session)
        let e = engine.clone();
        let db_ui_create = Arc::clone(&db);
        let sess_create = Arc::clone(&sessions);
        router.post("/ui/items", move |req| {
            let engine = e.clone();
            let pool = Arc::clone(&db_ui_create);
            let sessions = Arc::clone(&sess_create);
            async move {
                let token = req.header("cookie")
                    .and_then(|c| extract_session_cookie(Some(c)).map(str::to_owned));
                if token.as_deref().and_then(|t| sessions.get(t)).is_none() {
                    return ResponseBuilder::new(hyper::StatusCode::FOUND)
                        .header("location", "/ui/login")
                        .empty();
                }
                let body_str = String::from_utf8_lossy(&req.body).to_string();
                let mut name = String::new();
                let mut description = String::new();
                for pair in body_str.split('&') {
                    if let Some((k, v)) = pair.split_once('=') {
                        let val = v.replace('+', " ");
                        let decoded = percent_encoding::percent_decode_str(&val)
                            .decode_utf8_lossy()
                            .to_string();
                        if k == "name"        { name = decoded.clone(); }
                        if k == "description" { description = decoded; }
                    }
                }
                match db::create_item(&pool, &name, &description).await {
                    Ok(_) => ResponseBuilder::new(hyper::StatusCode::FOUND)
                        .header("location", "/ui/items?flash=Item+created")
                        .empty(),
                    Err(e) => {
                        error!(err = %e, "ui create_item failed");
                        let items = db::list_items(&pool).await.unwrap_or_default();
                        let mut ctx = tera::Context::new();
                        ctx.insert("items", &items);
                        ctx.insert("flash", "Failed to create item.");
                        ctx.insert("flash_type", "error");
                        engine.render("items.html", &ctx)
                    }
                }
            }
        });

        // POST /ui/items/:id/delete — delete item (requires session)
        let db_ui_delete = Arc::clone(&db);
        let sess_delete = Arc::clone(&sessions);
        router.post("/ui/items/:id/delete", move |req| {
            let pool = Arc::clone(&db_ui_delete);
            let sessions = Arc::clone(&sess_delete);
            async move {
                let token = req.header("cookie")
                    .and_then(|c| extract_session_cookie(Some(c)).map(str::to_owned));
                if token.as_deref().and_then(|t| sessions.get(t)).is_none() {
                    return ResponseBuilder::new(hyper::StatusCode::FOUND)
                        .header("location", "/ui/login")
                        .empty();
                }
                let id = req.path_param("id").unwrap_or("").to_string();
                match db::delete_item(&pool, &id).await {
                    Ok(_) => ResponseBuilder::new(hyper::StatusCode::FOUND)
                        .header("location", "/ui/items?flash=Item+deleted")
                        .empty(),
                    Err(e) => {
                        error!(err = %e, "ui delete_item failed");
                        ResponseBuilder::new(hyper::StatusCode::FOUND)
                            .header("location", "/ui/items")
                            .empty()
                    }
                }
            }
        });

        // GET /ui/metrics — HTML metrics dashboard
        let e = engine.clone();
        let m_ui = Arc::clone(&metrics);
        let sess_metrics = Arc::clone(&sessions);
        router.get("/ui/metrics", move |req| {
            let engine = e.clone();
            let m = Arc::clone(&m_ui);
            let sessions = Arc::clone(&sess_metrics);
            async move {
                let token = req.header("cookie")
                    .and_then(|c| extract_session_cookie(Some(c)).map(str::to_owned));
                let session_user = token.as_deref().and_then(|t| sessions.get(t));
                let mut ctx = tera::Context::new();
                if let Some(ref u) = session_user { ctx.insert("session_user", u); }
                ctx.insert("requests_total",       &m.requests_total.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("requests_active",      &m.requests_active.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("responses_2xx",        &m.responses_2xx.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("responses_4xx",        &m.responses_4xx.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("responses_5xx",        &m.responses_5xx.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("rate_limited",         &m.rate_limited.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("concurrency_limited",  &m.concurrency_limited.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("timed_out",            &m.timed_out.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("connections_accepted", &m.connections_accepted.load(std::sync::atomic::Ordering::Relaxed));
                ctx.insert("connections_rejected_ip", &m.connections_rejected_ip.load(std::sync::atomic::Ordering::Relaxed));
                engine.render("metrics.html", &ctx)
            }
        });
    }

    // GET /metrics — Prometheus-compatible metrics endpoint.
    //
    // Returns all server counters in the Prometheus text exposition format
    // (v0.0.4).  No authentication is required; restrict access via the IP
    // allowlist (ALLOWED_IPS) or a reverse proxy if needed.
    router.get("/metrics", move |_req| {
        let m = Arc::clone(&metrics);
        async move {
            let body = m.render();
            ResponseBuilder::ok()
                .header("content-type", "text/plain; version=0.0.4; charset=utf-8")
                .text(body)
        }
    });

    router
}

/// Compute the n-th Fibonacci number iteratively.
///
/// Intentionally CPU-bound (for large n) to demonstrate [`run_blocking`]
/// keeping async workers free.  Safe for all u64 n (returns 0 for n == 0,
/// wraps on overflow beyond fib(93) but the `/fib/:n` route caps at 50).
fn fib(n: u64) -> u64 {
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 0..n {
        (a, b) = (b, a.wrapping_add(b));
    }
    a
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::{Method, StatusCode};

    fn test_config() -> Arc<ServerConfig> {
        Arc::new(ServerConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            workers: 1,
            max_blocking_threads: 8,
            log_level: "info".to_string(),
            static_dir: "./static".to_string(),
            rate_limit_rps: 100,
            max_connections: 4,
            tls_cert_path: None,
            tls_key_path: None,
            http_redirect_port: None,
            max_body_bytes: 4_194_304,
            keep_alive_timeout_secs: 75,
            max_concurrent_requests: 5000,
            shutdown_drain_secs: 30,
            db_url: "sqlite::memory:".to_string(),
            jwt_secret: "test-secret".to_string(),
            auth_username: "admin".to_string(),
            auth_password: "secret".to_string(),
            request_timeout_secs: 30,
            db_pool_size: 5,
            blocked_ips: vec![],
            allowed_ips: vec![],
        })
    }

    async fn test_db() -> Arc<SqlitePool> {
        Arc::new(crate::db::init_pool("sqlite::memory:", 5).await.unwrap())
    }

    fn test_jwt() -> Arc<JwtSecret> {
        Arc::new(JwtSecret::new("test-secret"))
    }

    fn make_req(method: Method, uri: &str) -> HttpRequest {
        let (parts, _) = hyper::Request::builder()
            .method(method)
            .uri(uri)
            .body(())
            .unwrap()
            .into_parts();
        HttpRequest::from_parts(parts, Bytes::new())
    }

    async fn body_str(resp: HttpResponse) -> String {
        let bytes: Bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn health_route_returns_ok() {
        let cfg = test_config();
        let router = build_router(&cfg, test_db().await, test_jwt(), Metrics::new(), Arc::new(SessionStore::new()), None);
        let resp = router.dispatch(make_req(Method::GET, "/health")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_str(resp).await, "ok\n");
    }

    #[tokio::test]
    async fn root_route_returns_server_name() {
        let cfg = test_config();
        let router = build_router(&cfg, test_db().await, test_jwt(), Metrics::new(), Arc::new(SessionStore::new()), None);
        let resp = router.dispatch(make_req(Method::GET, "/")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_str(resp).await.contains("rust-highperf-server"));
    }

    #[tokio::test]
    async fn echo_path_param_returned() {
        let cfg = test_config();
        let router = build_router(&cfg, test_db().await, test_jwt(), Metrics::new(), Arc::new(SessionStore::new()), None);
        let resp = router.dispatch(make_req(Method::GET, "/echo/world")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_str(resp).await, "world\n");
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let cfg = test_config();
        let router = build_router(&cfg, test_db().await, test_jwt(), Metrics::new(), Arc::new(SessionStore::new()), None);
        let resp = router.dispatch(make_req(Method::GET, "/not-a-route")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn fib_known_values() {
        assert_eq!(fib(0), 0);
        assert_eq!(fib(1), 1);
        assert_eq!(fib(10), 55);
        assert_eq!(fib(20), 6765);
    }

    #[tokio::test]
    async fn fib_route_returns_correct_value() {
        let cfg = test_config();
        let router = build_router(&cfg, test_db().await, test_jwt(), Metrics::new(), Arc::new(SessionStore::new()), None);
        let resp = router.dispatch(make_req(Method::GET, "/fib/10")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_str(resp).await, "55\n");
    }

    #[tokio::test]
    async fn fib_route_caps_at_50() {
        let cfg = test_config();
        let router = build_router(&cfg, test_db().await, test_jwt(), Metrics::new(), Arc::new(SessionStore::new()), None);
        // n=999 should be capped to 50 → fib(50) = 12586269025
        let resp = router.dispatch(make_req(Method::GET, "/fib/999")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_str(resp).await, "12586269025\n");
    }

    #[tokio::test]
    async fn response_has_server_header() {
        let cfg = test_config();
        let router = build_router(&cfg, test_db().await, test_jwt(), Metrics::new(), Arc::new(SessionStore::new()), None);
        let resp = router.dispatch(make_req(Method::GET, "/health")).await;
        assert_eq!(
            resp.headers().get("server").unwrap(),
            "rust-highperf-server/0.1"
        );
    }

    #[tokio::test]
    async fn static_route_returns_404_for_missing_file() {
        let cfg = test_config();
        let router = build_router(&cfg, test_db().await, test_jwt(), Metrics::new(), Arc::new(SessionStore::new()), None);
        let resp = router
            .dispatch(make_req(Method::GET, "/static/nonexistent.txt"))
            .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
