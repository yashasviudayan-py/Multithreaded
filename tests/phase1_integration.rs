//! Phase 1 integration tests: TCP listener, accept loop, and HTTP responses.
//!
//! Tests spin up a real server on a random OS-assigned port and make actual
//! HTTP requests.  `reqwest` is used as the HTTP client.
//!
//! Run with `cargo test --test phase1_integration -- --test-threads=1` to
//! avoid resource exhaustion when multiple server + client pairs run in parallel.

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Spin up a server on a random port, run `f` with the *actual* bound address,
/// then shut down and wait for the server task to exit.
///
/// Key properties:
/// - The `TcpListener` is bound once and passed directly to `run_on_listener` —
///   no TOCTOU gap between binding and server start.
/// - Readiness is signalled via a channel; no `sleep` is needed.
/// - Shutdown errors are propagated (`expect`) so silent failures are visible.
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
        log_level: "info".to_string(),
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

    // Block until the server signals it is accepting connections — no sleep.
    let addr = ready_rx.await.expect("server did not signal readiness");

    f(addr).await;

    // `send` only fails if the receiver (server task) has already exited,
    // which means it crashed — surface that by letting `server_task.await` panic.
    let _ = shutdown_tx.send(());
    server_task.await.unwrap();
}

#[tokio::test]
async fn get_root_returns_200() {
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
async fn get_health_returns_ok() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/health"))
            .await
            .expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok\n");
    })
    .await;
}

#[tokio::test]
async fn response_has_server_header() {
    with_server(|addr| async move {
        let resp = reqwest::get(format!("http://{addr}/"))
            .await
            .expect("request failed");
        assert_eq!(
            resp.headers().get("server").unwrap().to_str().unwrap(),
            "rust-highperf-server/0.1"
        );
    })
    .await;
}

#[tokio::test]
async fn concurrent_connections_succeed() {
    with_server(|addr| async move {
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let url = format!("http://{addr}/health");
                tokio::spawn(
                    async move { reqwest::get(&url).await.expect("request failed").status() },
                )
            })
            .collect();

        for h in handles {
            assert_eq!(h.await.unwrap().as_u16(), 200);
        }
    })
    .await;
}
