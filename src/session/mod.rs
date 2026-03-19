//! Cookie-based session management.
//!
//! Sessions are stored server-side in a [`DashMap`] keyed by a random session
//! token.  The token is placed in a `Set-Cookie: session=<token>; HttpOnly;
//! SameSite=Strict` response header so it is inaccessible to JavaScript.
//!
//! The token itself is a UUID v4 (128 bits of randomness), not a signed value:
//! server-side storage means the token is meaningless without the in-memory map.
//! Tokens expire after [`SESSION_TTL`] of inactivity and are evicted by
//! [`SessionStore::evict_expired`], called on every access.
//!
//! # Usage
//! ```rust,ignore
//! // Create once at startup:
//! let sessions = Arc::new(SessionStore::new());
//!
//! // On login:
//! let token = sessions.create(username.clone());
//! let cookie = format!("session={token}; HttpOnly; SameSite=Strict; Path=/");
//!
//! // On each request:
//! if let Some(user) = sessions.get(&token) {
//!     // authenticated
//! }
//!
//! // On logout:
//! sessions.remove(&token);
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use uuid::Uuid;

/// How long a session stays alive without any activity.
pub const SESSION_TTL: Duration = Duration::from_secs(3600); // 1 hour

/// A single session slot.
struct Session {
    username: String,
    /// Timestamp of the last access; updated on every [`SessionStore::get`].
    last_accessed: Instant,
}

/// In-memory session store.
///
/// Create one instance per server startup and share via `Arc`.
pub struct SessionStore {
    sessions: DashMap<String, Session>,
    /// Call counter used to schedule periodic eviction sweeps.
    access_count: AtomicU64,
}

impl SessionStore {
    /// Create a new, empty session store.
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            access_count: AtomicU64::new(0),
        }
    }

    /// Create a new session for `username` and return its token.
    ///
    /// The token is a UUID v4 string suitable for use as a cookie value.
    pub fn create(&self, username: String) -> String {
        let token = Uuid::new_v4().to_string();
        self.sessions.insert(
            token.clone(),
            Session {
                username,
                last_accessed: Instant::now(),
            },
        );
        token
    }

    /// Look up a session by `token`.
    ///
    /// Returns the stored username if the session exists and is not expired.
    /// Refreshes `last_accessed` on a successful lookup.
    pub fn get(&self, token: &str) -> Option<String> {
        // Periodically evict stale sessions.
        let n = self.access_count.fetch_add(1, Ordering::Relaxed);
        if n > 0 && n.is_multiple_of(1_000) {
            self.evict_expired();
        }

        if let Some(mut entry) = self.sessions.get_mut(token) {
            if entry.last_accessed.elapsed() < SESSION_TTL {
                entry.last_accessed = Instant::now();
                return Some(entry.username.clone());
            }
        }
        None
    }

    /// Remove a session (logout).
    pub fn remove(&self, token: &str) {
        self.sessions.remove(token);
    }

    /// Evict all sessions whose `last_accessed` is older than [`SESSION_TTL`].
    pub fn evict_expired(&self) {
        self.sessions
            .retain(|_, s| s.last_accessed.elapsed() < SESSION_TTL);
    }

    /// Return the number of active sessions (including possibly stale ones not yet evicted).
    pub fn count(&self) -> usize {
        self.sessions.len()
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the `session` cookie value from a `Cookie:` header string.
///
/// Returns `None` if the header is absent or the cookie is not present.
pub fn extract_session_cookie(cookie_header: Option<&str>) -> Option<&str> {
    let header = cookie_header?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("session=") {
            return Some(val);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get_session() {
        let store = SessionStore::new();
        let token = store.create("alice".to_string());
        assert_eq!(store.get(&token), Some("alice".to_string()));
    }

    #[test]
    fn unknown_token_returns_none() {
        let store = SessionStore::new();
        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn remove_session() {
        let store = SessionStore::new();
        let token = store.create("bob".to_string());
        store.remove(&token);
        assert!(store.get(&token).is_none());
    }

    #[test]
    fn extract_session_cookie_finds_value() {
        let hdr = "theme=dark; session=abc123; lang=en";
        assert_eq!(extract_session_cookie(Some(hdr)), Some("abc123"));
    }

    #[test]
    fn extract_session_cookie_absent() {
        assert!(extract_session_cookie(Some("theme=dark")).is_none());
        assert!(extract_session_cookie(None).is_none());
    }
}
