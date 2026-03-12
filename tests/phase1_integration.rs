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

    let mut cfg = ServerConfig::from_env().unwrap();
    cfg.max_connections = 64;
    cfg.addr = listener.local_addr().unwrap(); // informational only

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (ready_tx, ready_rx) = oneshot::channel::<SocketAddr>();

    let server_task = tokio::spawn(async move {
        Server::new(cfg)
            .run_on_listener(
                listener,
                Some(ready_tx),
                async move { let _ = shutdown_rx.await; },
            )
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
                tokio::spawn(async move {
                    reqwest::get(&url).await.expect("request failed").status()
                })
            })
            .collect();

        for h in handles {
            assert_eq!(h.await.unwrap().as_u16(), 200);
        }
    })
    .await;
}
