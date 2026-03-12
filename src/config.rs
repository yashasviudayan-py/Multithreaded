//! Server configuration loaded from environment variables.
//!
//! All settings have sensible defaults so the server runs with zero config.
//! Copy `.env.example` to `.env` and override as needed for your environment.

use std::env;
use std::net::SocketAddr;
use thiserror::Error;

/// Top-level server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind the server to (HOST:PORT).
    pub addr: SocketAddr,
    /// Number of Tokio worker threads. Defaults to CPU core count.
    pub workers: usize,
    /// Log level filter string (e.g., `"info"`, `"debug"`, `"error"`).
    pub log_level: String,
    /// Directory to serve static files from.
    pub static_dir: String,
    /// Max requests per second per client IP (token-bucket rate limiter).
    pub rate_limit_rps: u32,
    /// Max concurrent connections before backpressure kicks in.
    pub max_connections: usize,
    /// Path to TLS certificate file. Phase 7: HTTPS.
    pub tls_cert_path: Option<String>,
    /// Path to TLS private key file. Phase 7: HTTPS.
    pub tls_key_path: Option<String>,
}

/// Errors that can occur when loading [`ServerConfig`].
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Invalid bind address '{0}'")]
    InvalidAddr(String),
    #[error("Invalid value for {0}: '{1}'")]
    InvalidValue(String, String),
}

impl ServerConfig {
    /// Load configuration from environment variables with sensible defaults.
    ///
    /// # Environment Variables
    /// | Variable          | Default       | Description                              |
    /// |-------------------|---------------|------------------------------------------|
    /// | `HOST`            | `0.0.0.0`     | Bind address                             |
    /// | `PORT`            | `8080`        | Bind port                                |
    /// | `WORKERS`         | CPU count     | Tokio worker thread count                |
    /// | `LOG_LEVEL`       | `info`        | Tracing log filter                       |
    /// | `STATIC_DIR`      | `./static`    | Static files directory                   |
    /// | `RATE_LIMIT_RPS`  | `100`         | Requests/sec per IP                      |
    /// | `MAX_CONNECTIONS` | `10000`       | Max concurrent connections               |
    /// | `TLS_CERT_PATH`   | —             | TLS cert path (Phase 7)                  |
    /// | `TLS_KEY_PATH`    | —             | TLS key path (Phase 7)                   |
    pub fn from_env() -> Result<Self, ConfigError> {
        let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

        let port_str = env::var("PORT").unwrap_or_else(|_| "8080".to_string());
        let port: u16 = port_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("PORT".into(), port_str.clone()))?;

        let addr_str = format!("{host}:{port}");
        let addr: SocketAddr = addr_str
            .parse()
            .map_err(|_| ConfigError::InvalidAddr(addr_str.clone()))?;

        let workers = match env::var("WORKERS") {
            Ok(v) => v
                .parse::<usize>()
                .map_err(|_| ConfigError::InvalidValue("WORKERS".into(), v))?,
            Err(_) => std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        };

        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let static_dir = env::var("STATIC_DIR").unwrap_or_else(|_| "./static".to_string());

        let rps_str = env::var("RATE_LIMIT_RPS").unwrap_or_else(|_| "100".to_string());
        let rate_limit_rps: u32 = rps_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("RATE_LIMIT_RPS".into(), rps_str.clone()))?;

        let conn_str = env::var("MAX_CONNECTIONS").unwrap_or_else(|_| "10000".to_string());
        let max_connections: usize = conn_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("MAX_CONNECTIONS".into(), conn_str.clone()))?;

        Ok(Self {
            addr,
            workers,
            log_level,
            static_dir,
            rate_limit_rps,
            max_connections,
            tls_cert_path: env::var("TLS_CERT_PATH").ok(),
            tls_key_path: env::var("TLS_KEY_PATH").ok(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Serialize env-mutating tests so they don't race each other.
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_load_without_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        for key in &["HOST", "PORT", "WORKERS", "LOG_LEVEL", "STATIC_DIR"] {
            env::remove_var(key);
        }
        let cfg = ServerConfig::from_env().expect("config should load with defaults");
        assert_eq!(cfg.addr.port(), 8080);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.static_dir, "./static");
        assert_eq!(cfg.rate_limit_rps, 100);
        assert_eq!(cfg.max_connections, 10000);
        assert!(cfg.tls_cert_path.is_none());
    }

    #[test]
    fn invalid_port_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("PORT", "notaport");
        let result = ServerConfig::from_env();
        env::remove_var("PORT");
        assert!(result.is_err());
    }
}
