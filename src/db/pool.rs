//! SQLite connection pool initialisation and schema migration.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr as _;
use thiserror::Error;

/// Errors that can occur while initialising the database pool.
#[derive(Debug, Error)]
pub enum DbError {
    /// An error from the underlying sqlx / SQLite layer.
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Create an SQLite connection pool and apply the initial schema migration.
///
/// Creates the database file if it does not already exist.  Safe to call on
/// every server start — the migration is idempotent (`CREATE TABLE IF NOT
/// EXISTS`).
pub async fn init_pool(db_url: &str) -> Result<SqlitePool, DbError> {
    // `create_if_missing(true)` ensures the SQLite file is created when the
    // URL points to a new path (e.g. on first launch).
    let opts = SqliteConnectOptions::from_str(db_url)?.create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await?;

    run_migrations(&pool).await?;
    Ok(pool)
}

/// Apply the baseline schema.  All statements use `IF NOT EXISTS` so they are
/// safe to run repeatedly on an existing database.
async fn run_migrations(pool: &SqlitePool) -> Result<(), DbError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS items (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            description TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}
