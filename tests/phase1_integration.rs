//! Phase 1 integration tests: TCP listener, accept loop, and HTTP responses.
//!
//! These tests spin up a real server on a random OS-assigned port, make actual
//! HTTP requests, and assert on the responses.  `reqwest` is used as the client.
//!
//! Run with `cargo test --test phase1_integration -- --test-threads=1` to avoid
//! resource exhaustion when all tests spin up concurrent servers simultaneously.

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;
use std::net::SocketAddr;
use tokio::sync::oneshot;

/// Start a server on a random port, run `f` with the bound address, then shut
/// down gracefully.  Returns once the server task exits.
async fn with_server<F, Fut>(f: F)
where
    F: FnOnce(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // Grab a free port from the OS.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let mut cfg = ServerConfig::from_env().unwrap();
    cfg.addr = addr;
    cfg.max_connections = 64;

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let server_task = tokio::spawn(async move {
        Server::new(cfg)
            .run_with_shutdown(async move { let _ = shutdown_rx.await; })
            .await
            .expect("server error");
    });

    // Give the server a moment to start listening.
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    f(addr).await;

    let _ = shutdown_tx.send(());
    server_task.await.unwrap();
}

#[tokio::test]
async fn get_root_returns_200() {
    with_server(|addr| async move {
        let url = format!("http://{addr}/");
        let resp = reqwest::get(&url).await.expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("rust-highperf-server"));
    })
    .await;
}

#[tokio::test]
async fn get_health_returns_ok() {
    with_server(|addr| async move {
        let url = format!("http://{addr}/health");
        let resp = reqwest::get(&url).await.expect("request failed");
        assert_eq!(resp.status().as_u16(), 200);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "ok\n");
    })
    .await;
}

#[tokio::test]
async fn response_has_server_header() {
    with_server(|addr| async move {
        let url = format!("http://{addr}/");
        let resp = reqwest::get(&url).await.expect("request failed");
        let server_header = resp.headers().get("server").unwrap().to_str().unwrap();
        assert_eq!(server_header, "rust-highperf-server/0.1");
    })
    .await;
}

#[tokio::test]
async fn concurrent_connections_succeed() {
    with_server(|addr| async move {
        let url = format!("http://{addr}/health");
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let u = url.clone();
                tokio::spawn(async move {
                    reqwest::get(&u).await.expect("request failed").status()
                })
            })
            .collect();

        for h in handles {
            let status = h.await.unwrap();
            assert_eq!(status.as_u16(), 200);
        }
    })
    .await;
}
