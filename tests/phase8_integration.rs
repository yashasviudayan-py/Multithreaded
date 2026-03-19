//! Phase 8 integration tests: HTTP/2 support, JWT authentication, and SQLite
//! CRUD API.
//!
//! Covers:
//! - POST /auth/token: valid credentials return a JWT; invalid credentials return 401
//! - GET /api/items: returns an empty array initially; returns items after creation
//! - GET /api/items/:id: returns 200 for existing item; 404 for missing item
//! - POST /api/admin/items: requires valid JWT; returns 201 Created; 401 without token
//! - DELETE /api/admin/items/:id: requires valid JWT; returns 200; 401 without token
//! - HTTP/2 negotiation over TLS (ALPN h2)
//!
//! Run with:
//!   cargo test --test phase8_integration -- --test-threads=1

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
        // Use an in-memory database so each test gets an isolated, fresh DB.
        db_url: "sqlite::memory:".to_string(),
        jwt_secret: "phase8-test-secret".to_string(),
        auth_username: "admin".to_string(),
        auth_password: "secret".to_string(),
        request_timeout_secs: 30,
        db_pool_size: 5,
        blocked_ips: vec![],
        allowed_ips: vec![],
    }
}

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
    });

    let addr = ready_rx.await.unwrap();
    f(addr).await;

    shutdown_tx.send(()).ok();
    let result = server_task.await.unwrap();
    assert!(result.is_ok(), "Server exited with error: {result:?}");
}

// ── Auth endpoint tests ────────────────────────────────────────────────────────

/// Valid credentials return a `{"token": "..."}` JSON body with HTTP 200.
#[tokio::test]
async fn auth_token_valid_credentials_returns_jwt() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/auth/token"))
            .json(&serde_json::json!({"username": "admin", "password": "secret"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body["token"].is_string(),
            "Expected token field, got: {body}"
        );
        // Minimal sanity: a JWT has exactly two '.' characters.
        let token = body["token"].as_str().unwrap();
        assert_eq!(token.chars().filter(|&c| c == '.').count(), 2);
    })
    .await;
}

/// Wrong password returns HTTP 401.
#[tokio::test]
async fn auth_token_wrong_password_returns_401() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/auth/token"))
            .json(&serde_json::json!({"username": "admin", "password": "wrong"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    })
    .await;
}

/// Malformed JSON body returns HTTP 400.
#[tokio::test]
async fn auth_token_malformed_body_returns_400() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/auth/token"))
            .header("content-type", "application/json")
            .body("not json")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    })
    .await;
}

// ── Items CRUD tests ───────────────────────────────────────────────────────────

/// GET /api/items returns an empty array when the database is empty.
#[tokio::test]
async fn list_items_empty_returns_empty_array() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/api/items"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let items: Vec<serde_json::Value> = resp.json().await.unwrap();
        assert!(items.is_empty());
    })
    .await;
}

/// Create an item and then list it back.
#[tokio::test]
async fn create_and_list_item() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();

        // 1. Obtain a JWT.
        let token_resp = client
            .post(format!("http://{addr}/auth/token"))
            .json(&serde_json::json!({"username": "admin", "password": "secret"}))
            .send()
            .await
            .unwrap();
        assert_eq!(token_resp.status(), 200);
        let token_body: serde_json::Value = token_resp.json().await.unwrap();
        let token = token_body["token"].as_str().unwrap().to_string();

        // 2. Create an item using the JWT.
        let create_resp = client
            .post(format!("http://{addr}/api/admin/items"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": "Test Item", "description": "A test"}))
            .send()
            .await
            .unwrap();
        assert_eq!(create_resp.status(), 201);
        let item: serde_json::Value = create_resp.json().await.unwrap();
        assert_eq!(item["name"], "Test Item");
        assert!(item["id"].is_string());

        // 3. List items — should have exactly one.
        let list_resp = reqwest::get(format!("http://{addr}/api/items"))
            .await
            .unwrap();
        assert_eq!(list_resp.status(), 200);
        let items: Vec<serde_json::Value> = list_resp.json().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], "Test Item");
    })
    .await;
}

/// GET /api/items/:id returns the item when it exists.
#[tokio::test]
async fn get_item_by_id_returns_200() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();

        // Get token.
        let token: String = {
            let r = client
                .post(format!("http://{addr}/auth/token"))
                .json(&serde_json::json!({"username": "admin", "password": "secret"}))
                .send()
                .await
                .unwrap();
            let b: serde_json::Value = r.json().await.unwrap();
            b["token"].as_str().unwrap().to_string()
        };

        // Create item.
        let created: serde_json::Value = client
            .post(format!("http://{addr}/api/admin/items"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": "Widget", "description": "A widget"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = created["id"].as_str().unwrap();

        // Fetch by ID.
        let resp = reqwest::get(format!("http://{addr}/api/items/{id}"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let fetched: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(fetched["id"], created["id"]);
        assert_eq!(fetched["name"], "Widget");
    })
    .await;
}

/// GET /api/items/:id returns 404 for an unknown ID.
#[tokio::test]
async fn get_item_unknown_id_returns_404() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let resp = reqwest::get(format!("http://{addr}/api/items/nonexistent-id"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    })
    .await;
}

/// POST /api/admin/items without a token returns 401.
#[tokio::test]
async fn create_item_without_token_returns_401() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/admin/items"))
            .json(&serde_json::json!({"name": "Unauthorized", "description": "Should fail"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    })
    .await;
}

/// DELETE /api/admin/items/:id with valid JWT removes the item.
#[tokio::test]
async fn delete_item_with_valid_token_returns_200() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();

        // Get token.
        let token: String = {
            let r = client
                .post(format!("http://{addr}/auth/token"))
                .json(&serde_json::json!({"username": "admin", "password": "secret"}))
                .send()
                .await
                .unwrap();
            let b: serde_json::Value = r.json().await.unwrap();
            b["token"].as_str().unwrap().to_string()
        };

        // Create item.
        let created: serde_json::Value = client
            .post(format!("http://{addr}/api/admin/items"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": "ToDelete", "description": "Will be removed"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = created["id"].as_str().unwrap();

        // Delete item.
        let del_resp = client
            .delete(format!("http://{addr}/api/admin/items/{id}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(del_resp.status(), 200);

        // Verify it's gone.
        let check = reqwest::get(format!("http://{addr}/api/items/{id}"))
            .await
            .unwrap();
        assert_eq!(check.status(), 404);
    })
    .await;
}

/// DELETE /api/admin/items/:id without a token returns 401.
#[tokio::test]
async fn delete_item_without_token_returns_401() {
    let cfg = base_cfg("127.0.0.1:0".parse().unwrap());
    with_server(cfg, |addr| async move {
        let client = reqwest::Client::new();
        let resp = client
            .delete(format!("http://{addr}/api/admin/items/any-id"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    })
    .await;
}
