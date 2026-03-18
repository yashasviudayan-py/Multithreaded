//! Application domain models and CRUD helpers for the SQLite `items` table.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

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
pub async fn list_items(pool: &SqlitePool) -> Result<Vec<Item>, sqlx::Error> {
    sqlx::query_as::<_, Item>("SELECT id, name, description FROM items ORDER BY rowid ASC")
        .fetch_all(pool)
        .await
}

/// Return the item with the given `id`, or `None` if it does not exist.
pub async fn get_item(pool: &SqlitePool, id: &str) -> Result<Option<Item>, sqlx::Error> {
    sqlx::query_as::<_, Item>("SELECT id, name, description FROM items WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Insert a new item and return the inserted record.
///
/// Generates a fresh UUID v4 for the item's primary key.
pub async fn create_item(
    pool: &SqlitePool,
    name: &str,
    description: &str,
) -> Result<Item, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO items (id, name, description) VALUES (?, ?, ?)")
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
pub async fn delete_item(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM items WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
