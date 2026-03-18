//! Database module: SQLite connection pool and domain models.
//!
//! The pool is initialised once at server startup via [`init_pool`] and shared
//! (via [`Arc`]) across all connection tasks.

pub mod models;
pub mod pool;

pub use models::{create_item, delete_item, get_item, list_items, CreateItem, Item};
pub use pool::{init_pool, DbError};
