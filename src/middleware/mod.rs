//! Middleware module: composable Tower-compatible middleware stack.

pub mod auth;
pub mod concurrency;
pub mod logging;
pub mod rate_limiter;

pub use auth::{extract_bearer, AuthError, Claims, JwtSecret};
pub use concurrency::ConcurrencyLimiterLayer;
pub use logging::LoggingLayer;
pub use rate_limiter::{RateLimiter, RateLimiterLayer};
