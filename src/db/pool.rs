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
//! The `postgres` feature selects the PostgreSQL pool; the default build uses
//! SQLite. This avoids compiling unrelated database drivers into production
//! binaries.

#[cfg(feature = "postgres")]
use sqlx::postgres::{PgPool, PgPoolOptions};
#[cfg(not(feature = "postgres"))]
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
#[cfg(not(feature = "postgres"))]
use std::str::FromStr as _;
use thiserror::Error;

/// Errors that can occur while initialising the database pool.
#[derive(Debug, Error)]
pub enum DbError {
    /// An error from the underlying sqlx / SQLite layer.
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// The requested database URL does not match the compiled driver.
    #[error("PostgreSQL support requires the 'postgres' feature flag (recompile with --features postgres)")]
    BackendMismatch,
}

/// Shared connection-pool type for the selected database backend.
#[cfg(feature = "postgres")]
pub type DbPool = PgPool;
/// Shared connection-pool type for the selected database backend.
#[cfg(not(feature = "postgres"))]
pub type DbPool = SqlitePool;

/// Create a database connection pool and apply the initial schema migration.
///
/// The selected backend is determined at compile time:
/// - `--features postgres` accepts a `postgres://…` or `postgresql://…` URL.
/// - the default build accepts a SQLite URL.
///
/// `pool_size` is the maximum number of simultaneous database connections.
/// Set via `DB_POOL_SIZE`; defaults to 5.
pub async fn init_pool(db_url: &str, pool_size: u32) -> Result<DbPool, DbError> {
    let is_postgres = db_url.starts_with("postgres://") || db_url.starts_with("postgresql://");
    if is_postgres != cfg!(feature = "postgres") {
        return Err(DbError::BackendMismatch);
    }

    #[cfg(feature = "postgres")]
    let pool = PgPoolOptions::new()
        .max_connections(pool_size)
        .connect(db_url)
        .await?;

    #[cfg(not(feature = "postgres"))]
    let pool = {
        // Every SQLite `:memory:` connection owns a separate database. Keeping
        // this pool at one connection preserves the expected shared schema for
        // tests and ephemeral development databases.
        let effective_pool_size = if db_url == "sqlite::memory:" {
            1
        } else {
            pool_size
        };
        let options = SqliteConnectOptions::from_str(db_url)?.create_if_missing(true);
        SqlitePoolOptions::new()
            .max_connections(effective_pool_size)
            .connect_with(options)
            .await?
    };
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Apply the baseline SQLite schema.  All statements use `IF NOT EXISTS` so
/// they are safe to run repeatedly on an existing database.
async fn run_migrations(pool: &DbPool) -> Result<(), DbError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS items (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            description TEXT NOT NULL,
            created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            username      TEXT PRIMARY KEY,
            password_hash TEXT NOT NULL,
            created_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            token      TEXT PRIMARY KEY,
            username   TEXT NOT NULL,
            csrf_token TEXT NOT NULL,
            expires_at BIGINT NOT NULL
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
    #[cfg(not(feature = "postgres"))]
    async fn postgres_url_returns_backend_mismatch_error() {
        let err = init_pool("postgres://localhost/testdb", 5)
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::BackendMismatch));
    }
}
