//! Phase 7 integration tests: HTTPS / TLS support.
//!
//! Covers:
//! - TLS server starts and accepts HTTPS connections (self-signed cert)
//! - Existing HTTP routes work correctly over HTTPS (/, /health, /echo/:msg)
//! - Invalid TLS cert/key path surfaces as a startup error
//! - HTTP→HTTPS redirect server returns `308 Permanent Redirect` with correct Location
//! - Plain HTTP mode (no TLS config) still functions normally
//!
//! Run with:
//!   cargo test --test phase7_integration -- --test-threads=1

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;
use std::io::Write as _;
use std::net::SocketAddr;
use tempfile::NamedTempFile;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

// ── Self-signed certificate generation ────────────────────────────────────────

/// Generate a self-signed certificate and key for `localhost`.
///
/// Returns `(cert_pem, key_pem)` as owned strings.  Uses `rcgen` so there is
/// no dependency on OpenSSL or any external tool.
fn generate_self_signed() -> (String, String) {
    let key_pair = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&key_pair)
        .unwrap();
    (cert.pem(), key_pair.serialize_pem())
}

/// Write PEM strings to `NamedTempFile`s and return them.
///
/// Caller must keep the returned files alive for the duration of the test;
/// dropping them removes the files from disk.
fn temp_cert_files(cert_pem: &str, key_pem: &str) -> (NamedTempFile, NamedTempFile) {
    let mut cert_file = NamedTempFile::new().unwrap();
    cert_file.write_all(cert_pem.as_bytes()).unwrap();
    cert_file.flush().unwrap();

    let mut key_file = NamedTempFile::new().unwrap();
    key_file.write_all(key_pem.as_bytes()).unwrap();
    key_file.flush().unwrap();

    (cert_file, key_file)
}

// ── Test helpers ──────────────────────────────────────────────────────────────

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
    }
}

/// Spawn a server on a free port and run `f` with the bound address.
///
/// The server is shut down cleanly after `f` returns.  Tests must run with
/// `--test-threads=1` to avoid resource exhaustion.
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
    });

    let addr = ready_rx.await.unwrap();
    f(addr).await;

    shutdown_tx.send(()).ok();
    let result = server_task.await.unwrap();
    assert!(result.is_ok(), "Server exited with error: {result:?}");
}

/// Build a `reqwest::Client` that skips TLS certificate validation.
///
/// Necessary for testing with self-signed certificates.
fn insecure_https_client() -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Plain HTTP server (no TLS config) still accepts and responds normally.
///
/// This is a quick smoke-test to ensure Phase 7 changes didn't break the
/// existing HTTP code path.
#[tokio::test]
async fn plain_http_health_check_still_works() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = base_cfg(addr);

    with_server(cfg, |addr| async move {
        let url = format!("http://{addr}/health");
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok\n");
    })
    .await;
}

/// HTTPS server returns 200 for `/health`.
#[tokio::test]
async fn https_health_check_returns_200() {
    let (cert_pem, key_pem) = generate_self_signed();
    let (cert_file, key_file) = temp_cert_files(&cert_pem, &key_pem);

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        tls_cert_path: Some(cert_file.path().to_str().unwrap().to_string()),
        tls_key_path: Some(key_file.path().to_str().unwrap().to_string()),
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        let client = insecure_https_client();
        let url = format!("https://localhost:{}/health", addr.port());
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok\n");
    })
    .await;

    // Keep temp files alive until server shuts down.
    drop(cert_file);
    drop(key_file);
}

/// HTTPS server returns 200 for `/` with the expected body.
#[tokio::test]
async fn https_root_route_returns_server_name() {
    let (cert_pem, key_pem) = generate_self_signed();
    let (cert_file, key_file) = temp_cert_files(&cert_pem, &key_pem);

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        tls_cert_path: Some(cert_file.path().to_str().unwrap().to_string()),
        tls_key_path: Some(key_file.path().to_str().unwrap().to_string()),
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        let client = insecure_https_client();
        let url = format!("https://localhost:{}/", addr.port());
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("rust-highperf-server"),
            "Unexpected body: {body}"
        );
    })
    .await;

    drop(cert_file);
    drop(key_file);
}

/// Path parameter routing works correctly over HTTPS.
#[tokio::test]
async fn https_echo_route_returns_path_param() {
    let (cert_pem, key_pem) = generate_self_signed();
    let (cert_file, key_file) = temp_cert_files(&cert_pem, &key_pem);

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        tls_cert_path: Some(cert_file.path().to_str().unwrap().to_string()),
        tls_key_path: Some(key_file.path().to_str().unwrap().to_string()),
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        let client = insecure_https_client();
        let url = format!("https://localhost:{}/echo/hello-tls", addr.port());
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "hello-tls\n");
    })
    .await;

    drop(cert_file);
    drop(key_file);
}

/// A request to an unknown path returns 404 over HTTPS.
#[tokio::test]
async fn https_unknown_path_returns_404() {
    let (cert_pem, key_pem) = generate_self_signed();
    let (cert_file, key_file) = temp_cert_files(&cert_pem, &key_pem);

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        tls_cert_path: Some(cert_file.path().to_str().unwrap().to_string()),
        tls_key_path: Some(key_file.path().to_str().unwrap().to_string()),
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        let client = insecure_https_client();
        let url = format!("https://localhost:{}/this-does-not-exist", addr.port());
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), 404);
    })
    .await;

    drop(cert_file);
    drop(key_file);
}

/// The `Server-` header is present on HTTPS responses (same as HTTP).
#[tokio::test]
async fn https_response_has_server_header() {
    let (cert_pem, key_pem) = generate_self_signed();
    let (cert_file, key_file) = temp_cert_files(&cert_pem, &key_pem);

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        tls_cert_path: Some(cert_file.path().to_str().unwrap().to_string()),
        tls_key_path: Some(key_file.path().to_str().unwrap().to_string()),
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        let client = insecure_https_client();
        let url = format!("https://localhost:{}/health", addr.port());
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let server_hdr = resp.headers().get("server").unwrap().to_str().unwrap();
        assert_eq!(server_hdr, "rust-highperf-server/0.1");
    })
    .await;

    drop(cert_file);
    drop(key_file);
}

/// A non-existent cert path causes `run_on_listener` to return a `Tls` error
/// before accepting any connection.
#[tokio::test]
async fn bad_cert_path_returns_tls_startup_error() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        tls_cert_path: Some("/nonexistent/cert.pem".to_string()),
        tls_key_path: Some("/nonexistent/key.pem".to_string()),
        ..base_cfg(addr)
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound = listener.local_addr().unwrap();
    let cfg = ServerConfig { addr: bound, ..cfg };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let result = Server::new(cfg)
        .run_on_listener(listener, None, async move {
            let _ = shutdown_rx.await;
        })
        .await;

    shutdown_tx.send(()).ok();

    assert!(
        result.is_err(),
        "Expected TLS error but server started successfully"
    );
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("TLS") || err_str.contains("cert") || err_str.contains("Failed"),
        "Unexpected error message: {err_str}"
    );
}

/// Only one of cert/key is set — server falls back to plain HTTP (no error).
#[tokio::test]
async fn only_cert_no_key_falls_back_to_plain_http() {
    let (cert_pem, _key_pem) = generate_self_signed();
    let (cert_file, _key_file) = temp_cert_files(&cert_pem, "");

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        // Only cert is set — key is missing.  Server should warn and fall back.
        tls_cert_path: Some(cert_file.path().to_str().unwrap().to_string()),
        tls_key_path: None,
        ..base_cfg(addr)
    };

    // Server should start successfully (plain HTTP fallback).
    with_server(cfg, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/health")).await.unwrap();
        assert_eq!(resp.status(), 200);
    })
    .await;
}

/// HTTP→HTTPS redirect server returns `308 Permanent Redirect`.
#[tokio::test]
async fn http_redirect_returns_308_with_location() {
    let (cert_pem, key_pem) = generate_self_signed();
    let (cert_file, key_file) = temp_cert_files(&cert_pem, &key_pem);

    // Reserve a free port for the redirect listener.
    let redirect_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirect_port = redirect_listener.local_addr().unwrap().port();
    drop(redirect_listener); // Release so the server can bind it.

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        tls_cert_path: Some(cert_file.path().to_str().unwrap().to_string()),
        tls_key_path: Some(key_file.path().to_str().unwrap().to_string()),
        http_redirect_port: Some(redirect_port),
        ..base_cfg(addr)
    };

    with_server(cfg, |tls_addr| async move {
        // Give the redirect server a moment to bind (it spawns inside accept_loop).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = reqwest::Client::builder()
            // Do NOT follow redirects — we want to inspect the 308 response itself.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        let url = format!("http://localhost:{redirect_port}/health");
        let resp = client.get(&url).send().await.unwrap();

        assert_eq!(resp.status(), 308);

        let location = resp
            .headers()
            .get("location")
            .expect("Missing Location header")
            .to_str()
            .unwrap();

        // Location must point to https:// on the TLS port.
        let expected_prefix = format!("https://localhost:{}", tls_addr.port());
        assert!(
            location.starts_with(&expected_prefix),
            "Expected Location to start with '{expected_prefix}', got: '{location}'"
        );
        assert!(
            location.contains("/health"),
            "Expected Location to contain '/health', got: '{location}'"
        );
    })
    .await;

    drop(cert_file);
    drop(key_file);
}

/// Multiple concurrent HTTPS requests are all handled correctly.
#[tokio::test]
async fn https_concurrent_requests_all_succeed() {
    let (cert_pem, key_pem) = generate_self_signed();
    let (cert_file, key_file) = temp_cert_files(&cert_pem, &key_pem);

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = ServerConfig {
        tls_cert_path: Some(cert_file.path().to_str().unwrap().to_string()),
        tls_key_path: Some(key_file.path().to_str().unwrap().to_string()),
        max_concurrent_requests: 50,
        ..base_cfg(addr)
    };

    with_server(cfg, |addr| async move {
        let client = std::sync::Arc::new(insecure_https_client());
        let port = addr.port();

        let tasks: Vec<_> = (0..10)
            .map(|i| {
                let client = std::sync::Arc::clone(&client);
                tokio::spawn(async move {
                    let url = format!("https://localhost:{port}/echo/req-{i}");
                    let resp = client.get(&url).send().await.unwrap();
                    assert_eq!(resp.status(), 200);
                    let body = resp.text().await.unwrap();
                    assert_eq!(body, format!("req-{i}\n"));
                })
            })
            .collect();

        for task in tasks {
            task.await.unwrap();
        }
    })
    .await;

    drop(cert_file);
    drop(key_file);
}
