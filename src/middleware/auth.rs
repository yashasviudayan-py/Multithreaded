//! JWT authentication helpers.
//!
//! [`JwtSecret`] wraps an HMAC-SHA256 encode/decode key pair and exposes two
//! methods:
//!
//! - [`JwtSecret::create_token`] — mint a short-lived token for a subject.
//! - [`JwtSecret::verify_token`] — validate a token and return its claims.
//!
//! Route handlers that require authentication call [`extract_bearer`] to pull
//! the raw token out of the `Authorization` header, then call
//! [`JwtSecret::verify_token`].  On failure the handler returns a `401
//! Unauthorized` response immediately; no Tower middleware is required.
//!
//! # Example
//! ```rust,ignore
//! let jwt = JwtSecret::new("my-secret");
//! let token = jwt.create_token("alice").unwrap();
//! let claims = jwt.verify_token(&token).unwrap();
//! assert_eq!(claims.sub, "alice");
//! ```

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// JWT token lifetime: one hour.
const TOKEN_TTL_SECS: u64 = 3600;

// ── Claims ────────────────────────────────────────────────────────────────────

/// Payload stored inside a signed JWT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — typically the username or user ID.
    pub sub: String,
    /// Expiry timestamp (seconds since UNIX epoch, UTC).
    pub exp: usize,
}

// ── JwtSecret ─────────────────────────────────────────────────────────────────

/// Holds HMAC-SHA256 encode and decode keys derived from a shared secret.
///
/// Create once at server startup and share via `Arc<JwtSecret>`.
pub struct JwtSecret {
    encoding: EncodingKey,
    decoding: DecodingKey,
}

/// Errors that can occur when creating a JWT.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The jsonwebtoken library rejected the operation.
    #[error("Token error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
}

impl JwtSecret {
    /// Create a new [`JwtSecret`] from a UTF-8 `secret` string.
    pub fn new(secret: &str) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
        }
    }

    /// Mint a new HS256 JWT for `username`.
    ///
    /// The token expires [`TOKEN_TTL_SECS`] seconds from now.
    pub fn create_token(&self, username: &str) -> Result<String, AuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let claims = Claims {
            sub: username.to_string(),
            exp: (now + TOKEN_TTL_SECS) as usize,
        };
        encode(&Header::default(), &claims, &self.encoding).map_err(AuthError::Jwt)
    }

    /// Validate `token` and return its [`Claims`] on success.
    ///
    /// Returns a jsonwebtoken error if the token is malformed, expired, or
    /// signed with a different secret.
    pub fn verify_token(&self, token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
        let validation = Validation::new(Algorithm::HS256);
        let token_data = decode::<Claims>(token, &self.decoding, &validation)?;
        Ok(token_data.claims)
    }
}

// ── Helper ────────────────────────────────────────────────────────────────────

/// Extract the raw Bearer token from the value of an `Authorization` header.
///
/// Returns `None` if the header is absent or does not start with `"Bearer "`.
pub fn extract_bearer(auth_header: Option<&str>) -> Option<&str> {
    auth_header?.strip_prefix("Bearer ")
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_token() {
        let jwt = JwtSecret::new("test-secret");
        let token = jwt.create_token("alice").unwrap();
        let claims = jwt.verify_token(&token).unwrap();
        assert_eq!(claims.sub, "alice");
    }

    #[test]
    fn wrong_secret_fails_verification() {
        let jwt1 = JwtSecret::new("secret-a");
        let jwt2 = JwtSecret::new("secret-b");
        let token = jwt1.create_token("bob").unwrap();
        assert!(jwt2.verify_token(&token).is_err());
    }

    #[test]
    fn extract_bearer_strips_prefix() {
        assert_eq!(extract_bearer(Some("Bearer abc.def")), Some("abc.def"));
        assert_eq!(extract_bearer(Some("Basic xyz")), None);
        assert_eq!(extract_bearer(None), None);
    }
}
