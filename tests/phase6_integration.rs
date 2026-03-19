//! Phase 6 integration tests: edge cases not covered by previous phases.
//!
//! Covers:
//! - Body at exactly the size limit (must succeed, not 413)
//! - Body exactly 1 byte over limit announced via Content-Length (must 413)
//! - Truncated HTTP connection (server must not hang)
//! - Rate-limiter bucket shared across multiple TCP connections from same IP
//! - Static file: directory without index.html returns 404
//! - Method-not-allowed at integration level (POST to GET-only route)
//! - Empty path segment / root variations
//!
//! Run with:
//!   cargo test --test phase6_integration -- --test-threads=1

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;
use std::net::SocketAddr;
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ── Shared test helper ────────────────────────────────────────────────────────

fn base_cfg(addr: SocketAddr) -> ServerConfig {
    ServerConfig {
        addr,
        workers: 2,
        max_blocking_threads: 8,
        log_level: "error".to_string(),
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
    }
}

async fn with_server<F, Fut>(cfg: ServerConfig, f: F)
where
    F: FnOnce(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound = listener.local_addr().unwrap();
    let cfg = ServerConfig { addr: bound, ..cfg };

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

// ── Body size boundary tests ───────────────────────────────────────────────────

/// Body at exactly the limit must be accepted (strict `>` check, not `>=`).
#[tokio::test]
async fn body_at_exact_limit_is_accepted() {
    let limit = 64usize;
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    let cfg = ServerConfig {
        max_body_bytes: limit,
        ..cfg
    };

    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();
        let body = vec![b'x'; limit];
        let resp = client
            .post(format!("http://{addr}/health"))
            .header("content-length", limit.to_string())
            .body(body)
            .send()
            .await
            .expect("request failed");
        // The body size equals the limit exactly — must NOT be rejected as 413.
        assert_ne!(
            resp.status().as_u16(),
            413,
            "body at exact limit should not be rejected"
        );
    })
    .await;
}

/// Body 1 byte over the limit announced via Content-Length must return 413.
#[tokio::test]
async fn body_one_byte_over_limit_returns_413() {
    let limit = 64usize;
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    let cfg = ServerConfig {
        max_body_bytes: limit,
        ..cfg
    };

    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();
        let body = vec![b'x'; limit + 1];
        let resp = client
            .post(format!("http://{addr}/health"))
            .header("content-length", (limit + 1).to_string())
            .body(body)
            .send()
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 413);
    })
    .await;
}

// ── Truncated / malformed HTTP ─────────────────────────────────────────────────

/// A client that drops the connection mid-request (no final CRLF) must not
/// hang the server.  Subsequent requests to the same server must still succeed.
#[tokio::test]
async fn truncated_connection_does_not_hang_server() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());

    with_server(cfg, |addr| async move {
        // Write an incomplete request — missing the final blank line.
        {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n")
                .await
                .unwrap();
            // Drop without sending the terminating `\r\n` — simulates a
            // client crash or network cut mid-request.
        }

        // Give hyper a moment to process the EOF and clean up.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // The server must still be alive and serving requests.
        let resp = reqwest::get(format!("http://{addr}/health"))
            .await
            .expect("server hung or died after truncated connection");
        assert_eq!(resp.status().as_u16(), 200);
    })
    .await;
}

/// A completely empty connection (immediate close) must be handled gracefully.
#[tokio::test]
async fn empty_connection_does_not_hang_server() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());

    with_server(cfg, |addr| async move {
        // Open and immediately close a TCP connection without sending anything.
        {
            let _stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let resp = reqwest::get(format!("http://{addr}/health"))
            .await
            .expect("server hung after empty connection");
        assert_eq!(resp.status().as_u16(), 200);
    })
    .await;
}

// ── Rate limiter across multiple TCP connections ───────────────────────────────

/// Multiple TCP connections from the same IP share one token bucket.
/// With a limit of 2 rps, a third back-to-back request must be rate-limited.
#[tokio::test]
async fn rate_limit_shared_across_connections() {
    let cfg = ServerConfig {
        rate_limit_rps: 2,
        ..base_cfg("127.0.0.1:0".parse().unwrap())
    };

    with_server(cfg, |addr| async move {
        // Use connection-pooling-disabled clients so each .get() opens a new
        // TCP socket, proving the bucket is shared at the IP level.
        let c1 = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();
        let c2 = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();
        let c3 = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();

        let r1 = c1
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(r1.status().as_u16(), 200);

        let r2 = c2
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(r2.status().as_u16(), 200);

        // Third request — bucket should be empty (issued back-to-back with no
        // time for token refill).
        let mut got_429 = false;
        for _ in 0..5 {
            let r = c3
                .get(format!("http://{addr}/health"))
                .send()
                .await
                .unwrap();
            if r.status().as_u16() == 429 {
                got_429 = true;
                break;
            }
        }
        assert!(
            got_429,
            "expected 429 when bucket is shared across TCP connections"
        );
    })
    .await;
}

// ── Static file edge cases ─────────────────────────────────────────────────────

/// A directory without an `index.html` file must return 404.
#[tokio::test]
async fn static_directory_without_index_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    // Create a subdirectory but do NOT put an index.html in it.
    let subdir = dir.path().join("emptydir");
    std::fs::create_dir(&subdir).unwrap();

    let cfg = ServerConfig {
        static_dir: dir.path().to_string_lossy().into_owned(),
        ..base_cfg("127.0.0.1:0".parse().unwrap())
    };

    with_server(cfg, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/static/emptydir"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 404);
    })
    .await;
}

// ── Method-not-allowed at integration level ───────────────────────────────────

/// POST to a GET-only route must return 405, not 404.
#[tokio::test]
async fn post_to_get_only_route_returns_405() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());

    with_server(cfg, |addr| async move {
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

/// DELETE to the root route must return 405.
#[tokio::test]
async fn delete_to_root_returns_405() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());

    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();
        let resp = client
            .delete(format!("http://{addr}/"))
            .send()
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 405);
    })
    .await;
}

// ── Response completeness ─────────────────────────────────────────────────────

/// Every response must include the `server` header with the expected value.
#[tokio::test]
async fn every_route_sends_server_header() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());

    with_server(cfg, |addr| async move {
        for path in ["/", "/health", "/echo/test", "/nonexistent"] {
            let resp = reqwest::get(format!("http://{addr}{path}"))
                .await
                .unwrap_or_else(|_| panic!("request to {path} failed"));
            let sv = resp
                .headers()
                .get("server")
                .unwrap_or_else(|| panic!("missing server header on {path}"))
                .to_str()
                .unwrap();
            assert_eq!(
                sv, "rust-highperf-server/0.1",
                "wrong server header on {path}"
            );
        }
    })
    .await;
}

/// The `content-length` header must always match the actual body byte count.
#[tokio::test]
async fn content_length_matches_body_on_all_routes() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());

    with_server(cfg, |addr| async move {
        for path in ["/health", "/echo/hello"] {
            let resp = reqwest::get(format!("http://{addr}{path}"))
                .await
                .unwrap_or_else(|_| panic!("request to {path} failed"));

            let declared: usize = resp
                .headers()
                .get("content-length")
                .unwrap_or_else(|| panic!("missing content-length on {path}"))
                .to_str()
                .unwrap()
                .parse()
                .unwrap();

            let body = resp.bytes().await.unwrap();
            assert_eq!(
                declared,
                body.len(),
                "content-length mismatch on {path}: header={declared}, actual={}",
                body.len()
            );
        }
    })
    .await;
}
