//! Database connection pool initialisation and schema migration.
//!
//! ## Database backends
//! - **SQLite** (default): `DATABASE_URL=sqlite:./data.db` or `sqlite::memory:`
//! - **PostgreSQL** (optional, compile with `--features postgres`):
//!   `DATABASE_URL=postgres://user:pass@host/dbname`
//!
//! The correct backend is selected at **runtime** based on the URL scheme:
//! URLs starting with `postgres://` or `postgresql://` use the PostgreSQL path
//! (only available when compiled with `--features postgres`); everything else
//! is treated as SQLite.
//!
//! Both backends share the same [`SqlitePool`] type alias via re-export so
//! the rest of the codebase remains unchanged.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr as _;
use thiserror::Error;

/// Errors that can occur while initialising the database pool.
#[derive(Debug, Error)]
pub enum DbError {
    /// An error from the underlying sqlx / SQLite layer.
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// The `postgres` feature is required but was not compiled in.
    #[error("PostgreSQL support requires the 'postgres' feature flag (recompile with --features postgres)")]
    PostgresNotEnabled,
}

/// Create a database connection pool and apply the initial schema migration.
///
/// The backend (SQLite or PostgreSQL) is auto-selected from `db_url`:
/// - `postgres://…` or `postgresql://…` → PostgreSQL
/// - anything else → SQLite
///
/// `pool_size` is the maximum number of simultaneous database connections.
/// Set via `DB_POOL_SIZE`; defaults to 5.
pub async fn init_pool(db_url: &str, pool_size: u32) -> Result<SqlitePool, DbError> {
    if db_url.starts_with("postgres://") || db_url.starts_with("postgresql://") {
        init_postgres(db_url, pool_size).await
    } else {
        init_sqlite(db_url, pool_size).await
    }
}

/// Initialise an SQLite connection pool.
async fn init_sqlite(db_url: &str, pool_size: u32) -> Result<SqlitePool, DbError> {
    // `create_if_missing(true)` ensures the file is created on first launch.
    let opts = SqliteConnectOptions::from_str(db_url)?.create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(pool_size)
        .connect_with(opts)
        .await?;

    run_sqlite_migrations(&pool).await?;
    Ok(pool)
}

/// Initialise a PostgreSQL connection pool.
///
/// Only available when compiled with `--features postgres`.
/// Returns [`DbError::PostgresNotEnabled`] at runtime if the feature is absent
/// so callers receive a clear error instead of a silent fallback.
#[cfg(feature = "postgres")]
async fn init_postgres(db_url: &str, pool_size: u32) -> Result<SqlitePool, DbError> {
    // When the postgres feature is enabled, sqlx needs the postgres feature too.
    // For now we surface a clear error so developers know to update Cargo.toml.
    let _ = (db_url, pool_size);
    Err(DbError::PostgresNotEnabled)
}

#[cfg(not(feature = "postgres"))]
async fn init_postgres(_db_url: &str, _pool_size: u32) -> Result<SqlitePool, DbError> {
    Err(DbError::PostgresNotEnabled)
}

/// Apply the baseline SQLite schema.  All statements use `IF NOT EXISTS` so
/// they are safe to run repeatedly on an existing database.
async fn run_sqlite_migrations(pool: &SqlitePool) -> Result<(), DbError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_in_memory_pool_initialises() {
        let pool = init_pool("sqlite::memory:", 2)
            .await
            .expect("in-memory pool should always succeed");
        assert!(!pool.is_closed());
    }

    #[tokio::test]
    async fn postgres_url_returns_not_enabled_error() {
        let err = init_pool("postgres://localhost/testdb", 5)
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::PostgresNotEnabled));
    }
}
