//! Database-backed cookie sessions.
//!
//! Keeping session records in the application database makes authenticated UI
//! requests work across restarts and across multiple server replicas that use
//! the same database. Cookies contain only random UUIDs; the user identity and
//! CSRF secret remain server-side.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::db::DbPool;

/// How long a session stays valid, in seconds.
pub const SESSION_TTL_SECS: u64 = 3600;

/// Server-side data associated with an authenticated browser session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Authenticated account name.
    pub username: String,
    /// Per-session token required for state-changing HTML form submissions.
    pub csrf_token: String,
}

/// Session store backed by the shared application database.
#[derive(Clone)]
pub struct SessionStore {
    pool: Arc<DbPool>,
}

impl SessionStore {
    /// Construct a store using the application's database pool.
    pub fn new(pool: Arc<DbPool>) -> Self {
        Self { pool }
    }

    /// Create a session and return its opaque cookie token.
    pub async fn create(&self, username: String) -> Result<String, sqlx::Error> {
        let token = Uuid::new_v4().to_string();
        let csrf_token = Uuid::new_v4().to_string();
        let expires_at = now_secs() + SESSION_TTL_SECS as i64;
        sqlx::query("INSERT INTO sessions (token, username, csrf_token, expires_at) VALUES ($1, $2, $3, $4)")
            .bind(&token)
            .bind(username)
            .bind(csrf_token)
            .bind(expires_at)
            .execute(&*self.pool)
            .await?;
        Ok(token)
    }

    /// Look up and refresh a session. Expired records are removed.
    pub async fn get(&self, token: &str) -> Result<Option<Session>, sqlx::Error> {
        let now = now_secs();
        let session: Option<(String, String)> = sqlx::query_as(
            "SELECT username, csrf_token FROM sessions WHERE token = $1 AND expires_at > $2",
        )
        .bind(token)
        .bind(now)
        .fetch_optional(&*self.pool)
        .await?;
        if session.is_none() {
            sqlx::query("DELETE FROM sessions WHERE token = $1")
                .bind(token)
                .execute(&*self.pool)
                .await?;
            return Ok(None);
        }
        sqlx::query("UPDATE sessions SET expires_at = $1 WHERE token = $2")
            .bind(now + SESSION_TTL_SECS as i64)
            .bind(token)
            .execute(&*self.pool)
            .await?;
        Ok(session.map(|(username, csrf_token)| Session {
            username,
            csrf_token,
        }))
    }

    /// Delete a session at logout.
    pub async fn remove(&self, token: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM sessions WHERE token = $1")
            .bind(token)
            .execute(&*self.pool)
            .await?;
        Ok(())
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Extract the `session` cookie value from a `Cookie:` header string.
pub fn extract_session_cookie(cookie_header: Option<&str>) -> Option<&str> {
    let header = cookie_header?;
    header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("session="))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn store() -> SessionStore {
        SessionStore::new(Arc::new(db::init_pool("sqlite::memory:", 2).await.unwrap()))
    }

    #[tokio::test]
    async fn create_and_get_session() {
        let store = store().await;
        let token = store.create("alice".to_string()).await.unwrap();
        assert_eq!(store.get(&token).await.unwrap().unwrap().username, "alice");
    }

    #[tokio::test]
    async fn remove_session() {
        let store = store().await;
        let token = store.create("bob".to_string()).await.unwrap();
        store.remove(&token).await.unwrap();
        assert!(store.get(&token).await.unwrap().is_none());
    }

    #[test]
    fn extract_session_cookie_finds_value() {
        assert_eq!(
            extract_session_cookie(Some("theme=dark; session=abc123; lang=en")),
            Some("abc123")
        );
    }
}
