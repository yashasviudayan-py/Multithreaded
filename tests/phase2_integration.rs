//! Phase 2 integration tests: HTTP parser, router, and response builder.
//!
//! Verifies end-to-end behaviour of the new routing layer introduced in Phase 2:
//! path-parameter extraction, 404 for unknown routes, 405 for wrong methods,
//! correct response headers (Content-Type, Content-Length, Server), and that
//! existing Phase 1 endpoints continue to work.
//!
//! Run with:
//!   cargo test --test phase2_integration -- --test-threads=1
//!
//! `--test-threads=1` prevents resource exhaustion when multiple server+client
//! pairs would otherwise run concurrently.

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ── Test helper ───────────────────────────────────────────────────────────────

/// Spin up a real server on a random OS-assigned port, run `f`, then shut down.
///
/// Identical to the helper in `phase1_integration.rs` — duplicated here so
/// each integration test file is self-contained and can run independently.
async fn with_server<F, Fut>(f: F)
where
    F: FnOnce(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

    let cfg = ServerConfig {
        addr: listener.local_addr().unwrap(),
        workers: 2,
        max_blocking_threads: 8,
        log_level: "error".to_string(), // suppress noise in test output
        static_dir: "./static".to_string(),
        rate_limit_rps: 1000,
        max_connections: 64,
        tls_cert_path: None,
        tls_key_path: None,
        http_redirect_port: None,
        max_body_bytes: 4_194_304,
        keep_alive_timeout_secs: 75,
        max_concurrent_requests: 5000,
        shutdown_drain_secs: 5,
        db_url: "sqlite::memory:".to_string(),
        jwt_secret: "test-secret".to_string(),
        auth_username: "admin".to_string(),
        auth_password: "secret".to_string(),
        request_timeout_secs: 30,
        db_pool_size: 5,
        blocked_ips: vec![],
        allowed_ips: vec![],
        proxy_upstream: None,
        proxy_strip_prefix: None,
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (ready_tx, ready_rx) = oneshot::channel::<SocketAddr>();

    let server_task = tokio::spawn(async move {
        Server::new(cfg)
            .run_on_listener(listener, Some(ready_tx), async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("server task failed");
    });

    let addr = ready_rx.await.expect("server did not signal readiness");

    f(addr).await;

    let _ = shutdown_tx.send(());
    server_task.await.unwrap();
}

// ── Backward-compatibility (Phase 1 routes still work) ───────────────────────

#[tokio::test]
async fn get_root_still_returns_200() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        assert!(resp.text().await.unwrap().contains("rust-highperf-server"));
    })
    .await;
}

#[tokio::test]
async fn get_health_still_returns_ok() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/health"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok\n");
    })
    .await;
}

// ── Phase 2: routing behaviour ────────────────────────────────────────────────

#[tokio::test]
async fn unknown_path_returns_404() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/no-such-route"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 404);
    })
    .await;
}

#[tokio::test]
async fn wrong_method_returns_405() {
    with_server(|addr| async move {
        // /health is registered for GET only; POST should yield 405.
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/health"))
            .send()
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 405);
    })
    .await;
}

#[tokio::test]
async fn echo_path_param_returned_in_body() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/echo/rustacean"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.text().await.unwrap(), "rustacean\n");
    })
    .await;
}

// ── Phase 2: response headers ─────────────────────────────────────────────────

#[tokio::test]
async fn response_includes_server_header() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/health"))
            .await
            .expect("request failed");
        assert_eq!(
            resp.headers()
                .get("server")
                .expect("missing server header")
                .to_str()
                .unwrap(),
            "rust-highperf-server/0.1"
        );
    })
    .await;
}

#[tokio::test]
async fn response_includes_content_type() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/health"))
            .await
            .expect("request failed");
        let ct = resp
            .headers()
            .get("content-type")
            .expect("missing content-type header")
            .to_str()
            .unwrap();
        assert!(ct.contains("text/plain"), "unexpected content-type: {ct}");
    })
    .await;
}

#[tokio::test]
async fn response_includes_content_length() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/health"))
            .await
            .expect("request failed");
        let cl: usize = resp
            .headers()
            .get("content-length")
            .expect("missing content-length header")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        // "ok\n" is 3 bytes
        assert_eq!(cl, 3);
    })
    .await;
}
