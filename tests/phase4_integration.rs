//! Phase 4 integration tests: middleware (logging, rate limiting) and static
//! file serving.
//!
//! Run with:
//!   cargo test --test phase4_integration -- --test-threads=1
//!
//! `--test-threads=1` prevents resource exhaustion when multiple server+client
//! pairs would otherwise run concurrently.

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ── Test helper ───────────────────────────────────────────────────────────────

/// Spin up a real server with a given `static_dir`, run `f`, then shut down.
async fn with_server_static<F, Fut>(static_dir: &str, f: F)
where
    F: FnOnce(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

    let cfg = ServerConfig {
        addr: listener.local_addr().unwrap(),
        workers: 2,
        max_blocking_threads: 8,
        log_level: "error".to_string(),
        static_dir: static_dir.to_string(),
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

/// Spin up a server with a rate limit of `rps` requests/second.
async fn with_rate_limited_server<F, Fut>(rps: u32, f: F)
where
    F: FnOnce(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

    let cfg = ServerConfig {
        addr: listener.local_addr().unwrap(),
        workers: 2,
        max_blocking_threads: 8,
        log_level: "error".to_string(),
        static_dir: "./static".to_string(),
        rate_limit_rps: rps,
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

/// Spin up a server with a body size limit of `max_bytes`.
async fn with_body_limited_server<F, Fut>(max_bytes: usize, f: F)
where
    F: FnOnce(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

    let cfg = ServerConfig {
        addr: listener.local_addr().unwrap(),
        workers: 2,
        max_blocking_threads: 8,
        log_level: "error".to_string(),
        static_dir: "./static".to_string(),
        rate_limit_rps: 1000,
        max_connections: 64,
        tls_cert_path: None,
        tls_key_path: None,
        http_redirect_port: None,
        max_body_bytes: max_bytes,
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

// ── Static file serving ───────────────────────────────────────────────────────

/// Create a temporary directory with test fixtures and return its path string.
fn make_test_static_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    std::fs::write(dir.path().join("hello.txt"), b"Hello, world!\n").unwrap();
    std::fs::write(dir.path().join("style.css"), b"body { margin: 0; }\n").unwrap();
    std::fs::write(
        dir.path().join("index.html"),
        b"<html><body>Index</body></html>\n",
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn static_file_served_with_correct_content_type() {
    let dir = make_test_static_dir();
    let static_path = dir.path().to_string_lossy().to_string();

    with_server_static(&static_path, |addr| async move {
        // .txt file → text/plain
        let resp = reqwest::get(format!("http://{addr}/static/hello.txt"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        let ct = resp
            .headers()
            .get("content-type")
            .expect("missing content-type")
            .to_str()
            .unwrap();
        assert!(ct.contains("text/plain"), "expected text/plain, got {ct}");
        assert_eq!(resp.text().await.unwrap(), "Hello, world!\n");
    })
    .await;
}

#[tokio::test]
async fn static_css_has_css_content_type() {
    let dir = make_test_static_dir();
    let static_path = dir.path().to_string_lossy().to_string();

    with_server_static(&static_path, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/static/style.css"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/css"), "expected text/css, got {ct}");
    })
    .await;
}

#[tokio::test]
async fn static_file_not_found_returns_404() {
    let dir = make_test_static_dir();
    let static_path = dir.path().to_string_lossy().to_string();

    with_server_static(&static_path, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/static/missing.txt"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 404);
    })
    .await;
}

#[tokio::test]
async fn static_path_traversal_blocked() {
    let dir = make_test_static_dir();
    let static_path = dir.path().to_string_lossy().to_string();

    with_server_static(&static_path, |addr| async move {
        // Encoded `..` traversal attempt — server must return 403 or 404.
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/static/..%2F..%2Fetc%2Fpasswd"))
            .send()
            .await
            .expect("request failed");
        let status = resp.status().as_u16();
        assert!(
            status == 403 || status == 404,
            "expected 403/404 for traversal, got {status}"
        );
    })
    .await;
}

#[tokio::test]
async fn static_directory_serves_index_html() {
    let dir = make_test_static_dir();
    let static_path = dir.path().to_string_lossy().to_string();

    // Create a subdir with its own index.html
    let subdir = dir.path().join("sub");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(subdir.join("index.html"), b"<html>Sub</html>\n").unwrap();

    with_server_static(&static_path, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/static/sub"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        assert!(resp.text().await.unwrap().contains("Sub"));
    })
    .await;
}

// ── Rate limiting ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn requests_within_rate_limit_succeed() {
    // Rate limit of 10 rps is well above our 2 sequential requests.
    with_rate_limited_server(10, |addr| async move {
        for _ in 0..2 {
            let resp = reqwest::get(format!("http://{addr}/health"))
                .await
                .expect("request failed");
            assert_eq!(resp.status().as_u16(), 200);
        }
    })
    .await;
}

#[tokio::test]
async fn rate_limit_returns_429_when_exceeded() {
    // Rate limit of 1 rps: after the first request the bucket is empty.
    with_rate_limited_server(1, |addr| async move {
        let client = reqwest::Client::new();

        // Burn the single token.
        let r1 = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("first request failed");
        assert_eq!(r1.status().as_u16(), 200);

        // Next request must be rate-limited.  Keep retrying until we get 429
        // (in case of sub-millisecond token refill rounding effects).
        let mut got_429 = false;
        for _ in 0..5 {
            let r = client
                .get(format!("http://{addr}/health"))
                .send()
                .await
                .expect("subsequent request failed");
            if r.status().as_u16() == 429 {
                got_429 = true;
                // Verify the retry-after header is present.
                assert!(
                    r.headers().get("retry-after").is_some(),
                    "429 response missing retry-after header"
                );
                break;
            }
        }
        assert!(got_429, "expected at least one 429 response");
    })
    .await;
}

// ── Body size limit ────────────────────────────────────────────────────────────

#[tokio::test]
async fn body_within_limit_accepted() {
    // Limit of 64 bytes; send 10 bytes.
    with_body_limited_server(64, |addr| async move {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/echo/test"))
            .body("short body")
            .send()
            .await
            .expect("request failed");
        // /echo/:message is GET-only, so this returns 405 — but the body was
        // accepted (not rejected for size).
        assert_ne!(resp.status().as_u16(), 413);
    })
    .await;
}

#[tokio::test]
async fn body_exceeding_content_length_limit_returns_413() {
    // Limit of 8 bytes.
    with_body_limited_server(8, |addr| async move {
        let client = reqwest::Client::new();
        // Send a body larger than the 8-byte limit with explicit Content-Length.
        let resp = client
            .post(format!("http://{addr}/health"))
            .header("content-length", "100")
            .body(vec![b'x'; 100])
            .send()
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 413);
    })
    .await;
}

// ── Backward compatibility (Phase 1/2/3 routes still work) ───────────────────

#[tokio::test]
async fn all_existing_routes_still_work() {
    with_server_static("./static", |addr| async move {
        let client = reqwest::Client::new();

        // GET /
        let r = client.get(format!("http://{addr}/")).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200);

        // GET /health
        let r = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 200);

        // GET /echo/:msg
        let r = client
            .get(format!("http://{addr}/echo/hi"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 200);
        assert_eq!(r.text().await.unwrap(), "hi\n");

        // GET /fib/5 → 5
        let r = client
            .get(format!("http://{addr}/fib/5"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 200);
        assert_eq!(r.text().await.unwrap(), "5\n");
    })
    .await;
}
