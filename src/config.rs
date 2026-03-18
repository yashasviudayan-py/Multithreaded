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
    /// Max threads in Tokio's blocking thread pool (`spawn_blocking`).
    ///
    /// These threads handle CPU-bound or blocking I/O work that must not
    /// run on the async worker threads.  Defaults to 512.
    pub max_blocking_threads: usize,
    /// Log level filter string (e.g., `"info"`, `"debug"`, `"error"`).
    pub log_level: String,
    /// Directory to serve static files from.
    pub static_dir: String,
    /// Max requests per second per client IP (token-bucket rate limiter).
    pub rate_limit_rps: u32,
    /// Max concurrent connections before backpressure kicks in.
    pub max_connections: usize,
    /// Path to TLS certificate file (PEM). Phase 7: HTTPS.
    pub tls_cert_path: Option<String>,
    /// Path to TLS private key file (PEM). Phase 7: HTTPS.
    pub tls_key_path: Option<String>,
    /// Port for the plain-HTTP → HTTPS redirect listener.
    ///
    /// When set (and TLS is configured), the server binds a second listener on
    /// this port that responds to every request with a `308 Permanent Redirect`
    /// pointing to the `https://` equivalent URL.  Typically set to `80` in
    /// production so that `http://` clients are automatically upgraded.
    pub http_redirect_port: Option<u16>,
    /// Maximum allowed request body size in bytes.
    ///
    /// Requests exceeding this limit receive a `413 Payload Too Large` response.
    /// Defaults to 4 MiB (4,194,304 bytes).
    pub max_body_bytes: usize,
    /// Idle keep-alive timeout in seconds.
    ///
    /// HTTP/1.1 connections with no in-flight request are closed after this
    /// many seconds of inactivity.  Defaults to 75 s (matches nginx default).
    pub keep_alive_timeout_secs: u64,
    /// Maximum number of requests processed concurrently across all connections.
    ///
    /// Requests beyond this limit receive an immediate `503 Service Unavailable`.
    /// This is a request-level limit; the connection-level limit is
    /// [`max_connections`].  Defaults to 5 000.
    pub max_concurrent_requests: usize,
    /// Graceful-shutdown drain timeout in seconds.
    ///
    /// After a shutdown signal the server stops accepting new connections and
    /// waits up to this many seconds for in-flight connections to close before
    /// forcing exit.  Defaults to 30 s.
    pub shutdown_drain_secs: u64,
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
    /// | Variable            | Default       | Description                              |
    /// |---------------------|---------------|------------------------------------------|
    /// | `HOST`              | `0.0.0.0`     | Bind address                             |
    /// | `PORT`              | `8080`        | Bind port                                |
    /// | `WORKERS`           | CPU count     | Tokio worker thread count                |
    /// | `BLOCKING_THREADS`  | `512`         | Tokio blocking thread pool size          |
    /// | `LOG_LEVEL`         | `info`        | Tracing log filter                       |
    /// | `STATIC_DIR`        | `./static`    | Static files directory                   |
    /// | `RATE_LIMIT_RPS`    | `100`         | Requests/sec per IP                      |
    /// | `MAX_CONNECTIONS`   | `10000`       | Max concurrent connections               |
    /// | `TLS_CERT_PATH`          | —             | TLS cert path (Phase 7)                  |
    /// | `TLS_KEY_PATH`           | —             | TLS key path (Phase 7)                   |
    /// | `HTTP_REDIRECT_PORT`     | —             | HTTP→HTTPS redirect port (Phase 7)       |
    /// | `MAX_BODY_BYTES`         | `4194304`     | Max request body size (bytes)            |
    /// | `KEEP_ALIVE_TIMEOUT`     | `75`          | Idle keep-alive timeout (seconds)        |
    /// | `MAX_CONCURRENT_REQUESTS`| `5000`        | Max in-flight requests server-wide       |
    /// | `SHUTDOWN_DRAIN_SECS`    | `30`          | Graceful-shutdown drain timeout (seconds)|
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
            Ok(v) => {
                let n = v
                    .parse::<usize>()
                    .map_err(|_| ConfigError::InvalidValue("WORKERS".into(), v.clone()))?;
                if n == 0 {
                    return Err(ConfigError::InvalidValue(
                        "WORKERS".into(),
                        "must be > 0".into(),
                    ));
                }
                n
            }
            Err(_) => std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        };

        let blocking_str = env::var("BLOCKING_THREADS").unwrap_or_else(|_| "512".to_string());
        let max_blocking_threads: usize = blocking_str.parse().map_err(|_| {
            ConfigError::InvalidValue("BLOCKING_THREADS".into(), blocking_str.clone())
        })?;
        if max_blocking_threads == 0 {
            return Err(ConfigError::InvalidValue(
                "BLOCKING_THREADS".into(),
                "must be > 0".into(),
            ));
        }

        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let static_dir = env::var("STATIC_DIR").unwrap_or_else(|_| "./static".to_string());

        let rps_str = env::var("RATE_LIMIT_RPS").unwrap_or_else(|_| "100".to_string());
        let rate_limit_rps: u32 = rps_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("RATE_LIMIT_RPS".into(), rps_str.clone()))?;
        if rate_limit_rps == 0 {
            return Err(ConfigError::InvalidValue(
                "RATE_LIMIT_RPS".into(),
                "must be > 0".into(),
            ));
        }

        let conn_str = env::var("MAX_CONNECTIONS").unwrap_or_else(|_| "10000".to_string());
        let max_connections: usize = conn_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("MAX_CONNECTIONS".into(), conn_str.clone()))?;
        if max_connections == 0 {
            return Err(ConfigError::InvalidValue(
                "MAX_CONNECTIONS".into(),
                "must be > 0".into(),
            ));
        }

        let body_str = env::var("MAX_BODY_BYTES").unwrap_or_else(|_| "4194304".to_string());
        let max_body_bytes: usize = body_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("MAX_BODY_BYTES".into(), body_str.clone()))?;
        if max_body_bytes == 0 {
            return Err(ConfigError::InvalidValue(
                "MAX_BODY_BYTES".into(),
                "must be > 0".into(),
            ));
        }

        let ka_str = env::var("KEEP_ALIVE_TIMEOUT").unwrap_or_else(|_| "75".to_string());
        let keep_alive_timeout_secs: u64 = ka_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("KEEP_ALIVE_TIMEOUT".into(), ka_str.clone()))?;
        if keep_alive_timeout_secs == 0 {
            return Err(ConfigError::InvalidValue(
                "KEEP_ALIVE_TIMEOUT".into(),
                "must be > 0".into(),
            ));
        }

        let concur_str = env::var("MAX_CONCURRENT_REQUESTS").unwrap_or_else(|_| "5000".to_string());
        let max_concurrent_requests: usize = concur_str.parse().map_err(|_| {
            ConfigError::InvalidValue("MAX_CONCURRENT_REQUESTS".into(), concur_str.clone())
        })?;
        if max_concurrent_requests == 0 {
            return Err(ConfigError::InvalidValue(
                "MAX_CONCURRENT_REQUESTS".into(),
                "must be > 0".into(),
            ));
        }

        let drain_str = env::var("SHUTDOWN_DRAIN_SECS").unwrap_or_else(|_| "30".to_string());
        let shutdown_drain_secs: u64 = drain_str.parse().map_err(|_| {
            ConfigError::InvalidValue("SHUTDOWN_DRAIN_SECS".into(), drain_str.clone())
        })?;
        if shutdown_drain_secs == 0 {
            return Err(ConfigError::InvalidValue(
                "SHUTDOWN_DRAIN_SECS".into(),
                "must be > 0".into(),
            ));
        }

        let http_redirect_port = match env::var("HTTP_REDIRECT_PORT") {
            Ok(v) => {
                let p: u16 = v.parse().map_err(|_| {
                    ConfigError::InvalidValue("HTTP_REDIRECT_PORT".into(), v.clone())
                })?;
                Some(p)
            }
            Err(_) => None,
        };

        Ok(Self {
            addr,
            workers,
            max_blocking_threads,
            log_level,
            static_dir,
            rate_limit_rps,
            max_connections,
            tls_cert_path: env::var("TLS_CERT_PATH").ok(),
            tls_key_path: env::var("TLS_KEY_PATH").ok(),
            http_redirect_port,
            max_body_bytes,
            keep_alive_timeout_secs,
            max_concurrent_requests,
            shutdown_drain_secs,
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
        for key in &[
            "HOST",
            "PORT",
            "WORKERS",
            "BLOCKING_THREADS",
            "LOG_LEVEL",
            "STATIC_DIR",
            "MAX_BODY_BYTES",
        ] {
            env::remove_var(key);
        }
        let cfg = ServerConfig::from_env().expect("config should load with defaults");
        assert_eq!(cfg.addr.port(), 8080);
        assert_eq!(cfg.max_blocking_threads, 512);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.static_dir, "./static");
        assert_eq!(cfg.rate_limit_rps, 100);
        assert_eq!(cfg.max_connections, 10000);
        assert_eq!(cfg.max_body_bytes, 4_194_304);
        assert_eq!(cfg.keep_alive_timeout_secs, 75);
        assert_eq!(cfg.max_concurrent_requests, 5000);
        assert_eq!(cfg.shutdown_drain_secs, 30);
        assert!(cfg.tls_cert_path.is_none());
    }

    #[test]
    fn zero_max_body_bytes_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("MAX_BODY_BYTES", "0");
        let result = ServerConfig::from_env();
        env::remove_var("MAX_BODY_BYTES");
        assert!(result.is_err());
    }

    #[test]
    fn zero_blocking_threads_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("BLOCKING_THREADS", "0");
        let result = ServerConfig::from_env();
        env::remove_var("BLOCKING_THREADS");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_port_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("PORT", "notaport");
        let result = ServerConfig::from_env();
        env::remove_var("PORT");
        assert!(result.is_err());
    }

    #[test]
    fn zero_max_connections_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("MAX_CONNECTIONS", "0");
        let result = ServerConfig::from_env();
        env::remove_var("MAX_CONNECTIONS");
        assert!(result.is_err());
    }

    #[test]
    fn zero_rate_limit_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("RATE_LIMIT_RPS", "0");
        let result = ServerConfig::from_env();
        env::remove_var("RATE_LIMIT_RPS");
        assert!(result.is_err());
    }

    #[test]
    fn zero_workers_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("WORKERS", "0");
        let result = ServerConfig::from_env();
        env::remove_var("WORKERS");
        assert!(result.is_err());
    }

    #[test]
    fn zero_keep_alive_timeout_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("KEEP_ALIVE_TIMEOUT", "0");
        let result = ServerConfig::from_env();
        env::remove_var("KEEP_ALIVE_TIMEOUT");
        assert!(result.is_err());
    }

    #[test]
    fn zero_max_concurrent_requests_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("MAX_CONCURRENT_REQUESTS", "0");
        let result = ServerConfig::from_env();
        env::remove_var("MAX_CONCURRENT_REQUESTS");
        assert!(result.is_err());
    }

    #[test]
    fn zero_shutdown_drain_secs_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("SHUTDOWN_DRAIN_SECS", "0");
        let result = ServerConfig::from_env();
        env::remove_var("SHUTDOWN_DRAIN_SECS");
        assert!(result.is_err());
    }
}
