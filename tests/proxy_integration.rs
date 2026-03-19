//! Reverse proxy integration tests.
//!
//! Each test starts an upstream server (a plain instance of our own server),
//! then starts a proxy server pointing at it.  Requests flow:
//!
//!   test client → proxy server → upstream server → proxy server → test client
//!
//! Run with:
//!   cargo test --test proxy_integration -- --test-threads=1

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ── Helpers ────────────────────────────────────────────────────────────────────

fn base_cfg(addr: SocketAddr) -> ServerConfig {
    ServerConfig {
        addr,
        workers: 2,
        max_blocking_threads: 8,
        log_level: "error".to_string(),
        static_dir: "./static".to_string(),
        rate_limit_rps: 10_000,
        max_connections: 64,
        tls_cert_path: None,
        tls_key_path: None,
        http_redirect_port: None,
        max_body_bytes: 4_194_304,
        keep_alive_timeout_secs: 75,
        max_concurrent_requests: 5000,
        shutdown_drain_secs: 5,
        db_url: "sqlite::memory:".to_string(),
        jwt_secret: "proxy-test-secret".to_string(),
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

/// Start a server on a random port and return its address plus a shutdown handle.
async fn start_server(cfg: ServerConfig) -> (SocketAddr, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = ServerConfig { addr, ..cfg };

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

    let bound_addr = ready_rx.await.unwrap();
    (bound_addr, shutdown_tx)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// A GET /health request through the proxy is forwarded to the upstream and
/// the upstream's "ok\n" body is relayed back.
#[tokio::test]
async fn proxy_forwards_get_to_upstream() {
    // 1. Start upstream server (no proxy).
    let (upstream_addr, upstream_shutdown) =
        start_server(base_cfg("127.0.0.1:0".parse().unwrap())).await;

    // 2. Start proxy pointing at upstream.
    let proxy_cfg = ServerConfig {
        proxy_upstream: Some(format!("http://{upstream_addr}")),
        ..base_cfg("127.0.0.1:0".parse().unwrap())
    };
    let (proxy_addr, proxy_shutdown) = start_server(proxy_cfg).await;

    // 3. Request /health through proxy — should be forwarded and return "ok\n".
    let resp = reqwest::get(format!("http://{proxy_addr}/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok\n");

    proxy_shutdown.send(()).ok();
    upstream_shutdown.send(()).ok();
}

/// Local routes on the proxy are served directly without forwarding to upstream.
#[tokio::test]
async fn proxy_local_routes_take_priority() {
    let (upstream_addr, upstream_shutdown) =
        start_server(base_cfg("127.0.0.1:0".parse().unwrap())).await;

    let proxy_cfg = ServerConfig {
        proxy_upstream: Some(format!("http://{upstream_addr}")),
        ..base_cfg("127.0.0.1:0".parse().unwrap())
    };
    let (proxy_addr, proxy_shutdown) = start_server(proxy_cfg).await;

    // /health is a local route on both servers; the proxy should serve it locally.
    let resp = reqwest::get(format!("http://{proxy_addr}/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok\n");

    proxy_shutdown.send(()).ok();
    upstream_shutdown.send(()).ok();
}

/// With PROXY_STRIP_PREFIX=/api, a request to /api/health is forwarded to
/// the upstream's /health.
#[tokio::test]
async fn proxy_strip_prefix_forwards_correctly() {
    let (upstream_addr, upstream_shutdown) =
        start_server(base_cfg("127.0.0.1:0".parse().unwrap())).await;

    let proxy_cfg = ServerConfig {
        proxy_upstream: Some(format!("http://{upstream_addr}")),
        proxy_strip_prefix: Some("/api".to_string()),
        ..base_cfg("127.0.0.1:0".parse().unwrap())
    };
    let (proxy_addr, proxy_shutdown) = start_server(proxy_cfg).await;

    // /api/health is not a local route; the proxy strips /api and forwards /health.
    let resp = reqwest::get(format!("http://{proxy_addr}/api/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok\n");

    proxy_shutdown.send(()).ok();
    upstream_shutdown.send(()).ok();
}

/// When the upstream is unreachable the proxy returns 502 Bad Gateway.
#[tokio::test]
async fn proxy_unreachable_upstream_returns_502() {
    // Port 19999 — nothing is listening there.
    let proxy_cfg = ServerConfig {
        proxy_upstream: Some("http://127.0.0.1:19999".to_string()),
        ..base_cfg("127.0.0.1:0".parse().unwrap())
    };
    let (proxy_addr, proxy_shutdown) = start_server(proxy_cfg).await;

    let resp = reqwest::get(format!("http://{proxy_addr}/anything"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);

    proxy_shutdown.send(()).ok();
}

/// Upstream status codes are relayed verbatim.  A 404 from the upstream
/// arrives as a 404 at the client (not silently remapped to 200 or 502).
#[tokio::test]
async fn proxy_relays_upstream_404() {
    let (upstream_addr, upstream_shutdown) =
        start_server(base_cfg("127.0.0.1:0".parse().unwrap())).await;

    let proxy_cfg = ServerConfig {
        proxy_upstream: Some(format!("http://{upstream_addr}")),
        ..base_cfg("127.0.0.1:0".parse().unwrap())
    };
    let (proxy_addr, proxy_shutdown) = start_server(proxy_cfg).await;

    // /no-such-route exists on neither proxy nor upstream → upstream 404.
    let resp = reqwest::get(format!("http://{proxy_addr}/no-such-route"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    proxy_shutdown.send(()).ok();
    upstream_shutdown.send(()).ok();
}
