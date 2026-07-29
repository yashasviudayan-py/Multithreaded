//! PostgreSQL compatibility test.
//!
//! Set `TEST_DATABASE_URL` and enable the `postgres` feature to run this
//! against a real server. The CI workflow supplies a disposable PostgreSQL 16
//! service; local development can use `docker compose up postgres`.

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_pool_runs_migrations_and_crud() {
    use rust_highperf_server::db;

    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: TEST_DATABASE_URL is unset");
        return;
    };
    let pool = db::init_pool(&url, 2).await.expect("connect to PostgreSQL");
    let username = format!("ci-{}", uuid::Uuid::new_v4());
    db::create_user_if_missing(&pool, &username, "test-password")
        .await
        .expect("create Argon2id user");
    assert!(db::verify_user(&pool, &username, "test-password")
        .await
        .expect("verify user"));

    let item = db::create_item(&pool, "postgres item", "stored through AnyPool")
        .await
        .expect("create item");
    assert_eq!(
        db::get_item(&pool, &item.id)
            .await
            .expect("read item")
            .expect("item exists")
            .name,
        "postgres item"
    );
    assert!(db::delete_item(&pool, &item.id).await.expect("delete item"));
}

#[cfg(not(feature = "postgres"))]
#[test]
fn postgres_feature_is_required() {
    // The test target remains visible in default builds but intentionally does
    // not try to connect without the optional driver.
}
