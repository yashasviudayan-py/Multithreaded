//! Database module: SQLite/PostgreSQL connection pool and domain models.
//!
//! The pool is initialised once at server startup via [`init_pool`] and shared
//! (via [`Arc`]) across all connection tasks.

pub mod models;
pub mod pool;

pub use models::{
    create_item, create_user_if_missing, create_user_with_hash_if_missing, delete_item, get_item,
    list_items, verify_user, CreateItem, Item,
};
pub use pool::{init_pool, DbError, DbPool};
