//! Phase 5 integration tests: concurrency limiter, graceful shutdown,
//! static file streaming, and keep-alive connection reuse.
//!
//! Run with:
//!   cargo test --test phase5_integration -- --test-threads=1
//!
//! `--test-threads=1` prevents resource exhaustion when multiple server+client
//! pairs run concurrently.

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Construct a baseline test config for `addr`.
fn base_cfg(addr: SocketAddr) -> ServerConfig {
    ServerConfig {
        addr,
        workers: 2,
        max_blocking_threads: 8,
        log_level: "error".to_string(),
        static_dir: "./static".to_string(),
        rate_limit_rps: 10_000,
        max_connections: 256,
        tls_cert_path: None,
        tls_key_path: None,
        http_redirect_port: None,
        max_body_bytes: 4_194_304,
        keep_alive_timeout_secs: 75,
        max_concurrent_requests: 5000,
        shutdown_drain_secs: 5,
    }
}

/// Spin up a server with `cfg`, run `f(addr)`, then shut it down.
async fn with_server<F, Fut>(cfg: ServerConfig, f: F)
where
    F: FnOnce(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = ServerConfig { addr, ..cfg };

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

// ── Concurrency limiter ────────────────────────────────────────────────────────

/// When `max_concurrent_requests` is 1 and we hammer the server with many
/// concurrent requests that go through `spawn_blocking` (an async yield point),
/// the concurrency limiter must reject the overflow with 503.
///
/// Using a multi-thread test runtime ensures tasks run in true parallel,
/// making the semaphore contention deterministic. The `/fib/:n` route
/// yields at `spawn_blocking().await`, so the permit is held long enough
/// for concurrent requests to race.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_limiter_returns_503_when_saturated() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cfg = ServerConfig {
        addr,
        max_concurrent_requests: 1,
        ..base_cfg(addr)
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (ready_tx, ready_rx) = oneshot::channel::<SocketAddr>();

    tokio::spawn(async move {
        Server::new(cfg)
            .run_on_listener(listener, Some(ready_tx), async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let addr = ready_rx.await.unwrap();

    // Use a client with no connection pooling so each request gets its own
    // TCP connection, maximising true concurrency at the server.
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();

    // Fire 30 concurrent requests to /fib/10.  The handler yields at
    // spawn_blocking().await which means the concurrency permit is held while
    // other tasks race to acquire it.  With cap=1 the vast majority must 503.
    let futs: Vec<_> = (0..30)
        .map(|_| {
            let c = client.clone();
            let url = format!("http://{addr}/fib/10");
            tokio::spawn(async move { c.get(url).send().await.unwrap().status().as_u16() })
        })
        .collect();

    let statuses: Vec<u16> = futures_util::future::join_all(futs)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let got_503 = statuses.iter().any(|&s| s == 503);
    assert!(
        got_503,
        "expected at least one 503 with concurrency cap=1; got: {statuses:?}"
    );

    let _ = shutdown_tx.send(());
}

/// 503 responses carry the `retry-after: 1` header.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_503_has_retry_after_header() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cfg = ServerConfig {
        addr,
        max_concurrent_requests: 1,
        ..base_cfg(addr)
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (ready_tx, ready_rx) = oneshot::channel::<SocketAddr>();

    tokio::spawn(async move {
        Server::new(cfg)
            .run_on_listener(listener, Some(ready_tx), async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let addr = ready_rx.await.unwrap();
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();

    // Hammer until we get a 503 with the retry-after header.
    let mut found_503 = false;
    'outer: for _ in 0..20 {
        let futs: Vec<_> = (0..15)
            .map(|_| {
                let c = client.clone();
                let url = format!("http://{addr}/fib/10");
                tokio::spawn(async move { c.get(url).send().await.unwrap() })
            })
            .collect();

        for resp in futures_util::future::join_all(futs)
            .await
            .into_iter()
            .filter_map(|r| r.ok())
        {
            if resp.status().as_u16() == 503 {
                assert_eq!(
                    resp.headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok()),
                    Some("1"),
                    "503 missing retry-after header"
                );
                found_503 = true;
                break 'outer;
            }
        }
    }

    assert!(found_503, "never got a 503 with concurrency cap=1");
    let _ = shutdown_tx.send(());
}

// ── Static file streaming ─────────────────────────────────────────────────────

/// A large file (> 64 KiB) is served in full via streaming.
#[tokio::test]
async fn large_static_file_served_completely() {
    let dir = tempfile::tempdir().unwrap();
    // Write a 512 KiB file of known bytes so we can verify completeness.
    let data: Vec<u8> = (0u8..=255).cycle().take(512 * 1024).collect();
    std::fs::write(dir.path().join("big.bin"), &data).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let static_path = dir.path().to_string_lossy().to_string();

    let cfg = ServerConfig {
        addr,
        static_dir: static_path,
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/static/big.bin"))
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        // Content-Length must equal our file size.
        let cl: u64 = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .expect("content-length header missing or invalid");
        assert_eq!(cl, 512 * 1024);

        // Body must match byte-for-byte.
        let body = resp.bytes().await.unwrap();
        assert_eq!(body.len(), 512 * 1024);
        assert_eq!(body.as_ref(), data.as_slice());
    })
    .await;
}

/// Content-Length header matches the actual file size for a small file.
#[tokio::test]
async fn static_file_content_length_correct() {
    let dir = tempfile::tempdir().unwrap();
    let content = b"Hello, streaming world!\n";
    std::fs::write(dir.path().join("hello.txt"), content).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cfg = ServerConfig {
        addr,
        static_dir: dir.path().to_string_lossy().to_string(),
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/static/hello.txt"))
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let cl: usize = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .expect("content-length missing");
        assert_eq!(cl, content.len());
        assert_eq!(resp.text().await.unwrap().as_bytes(), content);
    })
    .await;
}

// ── Graceful shutdown ─────────────────────────────────────────────────────────

/// After the shutdown signal the server stops accepting new connections, but
/// any in-flight request completes successfully.
///
/// We send the shutdown while the fib computation is running (it is
/// deliberately slow for large n), then verify the response still arrives.
#[tokio::test]
async fn graceful_shutdown_completes_in_flight_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cfg = ServerConfig {
        addr,
        shutdown_drain_secs: 10,
        ..base_cfg(addr)
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (ready_tx, ready_rx) = oneshot::channel::<SocketAddr>();

    let server_task = tokio::spawn(async move {
        Server::new(cfg)
            .run_on_listener(listener, Some(ready_tx), async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let addr = ready_rx.await.unwrap();
    let client = reqwest::Client::new();

    // Kick off a slow (CPU-bound) request that will be in-flight when we
    // send the shutdown signal.
    let resp_fut = tokio::spawn({
        let c = client.clone();
        async move { c.get(format!("http://{addr}/fib/45")).send().await }
    });

    // Give the request time to start processing, then trigger shutdown.
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    let _ = shutdown_tx.send(());

    // The in-flight request must complete with the correct answer.
    let resp = resp_fut.await.unwrap().expect("request failed");
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    // fib(45) = 1134903170
    assert_eq!(body.trim(), "1134903170");

    server_task.await.unwrap();
}

// ── Keep-alive + backward compatibility ───────────────────────────────────────

/// A single HTTP client can reuse one TCP connection for multiple requests
/// (keep-alive).  All Phase 1–4 routes must work.
#[tokio::test]
async fn keep_alive_connection_reuse() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), b"keepalive ok\n").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cfg = ServerConfig {
        addr,
        static_dir: dir.path().to_string_lossy().to_string(),
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        // reqwest uses a connection pool, so sequential requests on the same
        // Client instance reuse the same TCP connection (HTTP/1.1 keep-alive).
        let client = reqwest::Client::new();

        let r1 = client.get(format!("http://{addr}/")).send().await.unwrap();
        assert_eq!(r1.status().as_u16(), 200);

        let r2 = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(r2.status().as_u16(), 200);

        let r3 = client
            .get(format!("http://{addr}/echo/phase5"))
            .send()
            .await
            .unwrap();
        assert_eq!(r3.status().as_u16(), 200);
        assert_eq!(r3.text().await.unwrap(), "phase5\n");

        let r4 = client
            .get(format!("http://{addr}/static/test.txt"))
            .send()
            .await
            .unwrap();
        assert_eq!(r4.status().as_u16(), 200);
        assert_eq!(r4.text().await.unwrap(), "keepalive ok\n");
    })
    .await;
}
