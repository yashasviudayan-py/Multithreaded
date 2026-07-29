//! Application domain models and CRUD helpers for the shared database schema.

use crate::db::DbPool;
use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use serde::{Deserialize, Serialize};

/// A single item stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Item {
    /// Unique item identifier (UUID v4).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Optional longer description.
    pub description: String,
}

/// Payload accepted by the `POST /api/admin/items` route.
#[derive(Debug, Deserialize)]
pub struct CreateItem {
    /// Display name for the new item.
    pub name: String,
    /// Description for the new item.
    pub description: String,
}

/// Return all items ordered by insertion time (ascending).
pub async fn list_items(pool: &DbPool) -> Result<Vec<Item>, sqlx::Error> {
    sqlx::query_as::<_, Item>(
        "SELECT id, name, description FROM items ORDER BY created_at ASC, id ASC",
    )
    .fetch_all(pool)
    .await
}

/// Return the item with the given `id`, or `None` if it does not exist.
pub async fn get_item(pool: &DbPool, id: &str) -> Result<Option<Item>, sqlx::Error> {
    sqlx::query_as::<_, Item>("SELECT id, name, description FROM items WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Insert a new item and return the inserted record.
///
/// Generates a fresh UUID v4 for the item's primary key.
pub async fn create_item(
    pool: &DbPool,
    name: &str,
    description: &str,
) -> Result<Item, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO items (id, name, description) VALUES ($1, $2, $3)")
        .bind(&id)
        .bind(name)
        .bind(description)
        .execute(pool)
        .await?;
    Ok(Item {
        id,
        name: name.to_string(),
        description: description.to_string(),
    })
}

/// Delete the item with `id`.  Returns `true` if a row was removed.
pub async fn delete_item(pool: &DbPool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM items WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Create the bootstrap administrator if it is not already present.
///
/// Only an Argon2id hash is persisted. The bootstrap password is never stored
/// in plaintext and is not used again after the account exists.
pub async fn create_user_if_missing(
    pool: &DbPool,
    username: &str,
    bootstrap_password: &str,
) -> Result<(), sqlx::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(bootstrap_password.as_bytes(), &salt)
        .map_err(|e| sqlx::Error::Protocol(e.to_string()))?
        .to_string();
    create_user_with_hash_if_missing(pool, username, &hash).await
}

/// Create the bootstrap administrator using a pre-generated Argon2id hash.
///
/// This lets production deployments avoid placing an administrator password in
/// the environment at all.
pub async fn create_user_with_hash_if_missing(
    pool: &DbPool,
    username: &str,
    password_hash: &str,
) -> Result<(), sqlx::Error> {
    PasswordHash::new(password_hash)
        .map_err(|e| sqlx::Error::Protocol(format!("invalid Argon2 password hash: {e}")))?;
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT username FROM users WHERE username = $1")
            .bind(username)
            .fetch_optional(pool)
            .await?;
    if exists.is_some() {
        return Ok(());
    }
    sqlx::query("INSERT INTO users (username, password_hash) VALUES ($1, $2)")
        .bind(username)
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Validate a supplied password against the stored Argon2id hash.
pub async fn verify_user(
    pool: &DbPool,
    username: &str,
    password: &str,
) -> Result<bool, sqlx::Error> {
    let stored: Option<(String,)> =
        sqlx::query_as("SELECT password_hash FROM users WHERE username = $1")
            .bind(username)
            .fetch_optional(pool)
            .await?;
    let Some((hash,)) = stored else {
        return Ok(false);
    };
    let Ok(parsed) = PasswordHash::new(&hash) else {
        return Ok(false);
    };
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}
