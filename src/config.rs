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
    /// SQLite database URL (e.g., `sqlite:./data.db` or `sqlite::memory:`).
    ///
    /// Used by Phase 8 to store application data.  Defaults to
    /// `sqlite:./data.db`.
    pub db_url: String,
    /// Secret key used to sign and verify JSON Web Tokens (JWT).
    ///
    /// **Must be changed in production.**  Defaults to a placeholder value
    /// that will cause tests to warn loudly if left as-is.
    pub jwt_secret: String,
    /// Username accepted by the `/auth/token` endpoint.
    ///
    /// Loaded from `AUTH_USERNAME`; defaults to `"admin"`.  Change in
    /// production or replace with a database-backed user lookup.
    pub auth_username: String,
    /// Password accepted by the `/auth/token` endpoint.
    ///
    /// Loaded from `AUTH_PASSWORD`; defaults to `"secret"`.  **Must be
    /// changed in production.**
    pub auth_password: String,
    /// Per-request processing timeout in seconds.
    ///
    /// If a handler takes longer than this to produce a response, the server
    /// returns `503 Service Unavailable` and logs a warning.  Defaults to
    /// 30 s.  Set via `REQUEST_TIMEOUT_SECS`.
    pub request_timeout_secs: u64,
    /// Maximum number of SQLite connections in the pool.
    ///
    /// Set via `DB_POOL_SIZE`; defaults to `5`.  Increase for higher
    /// database concurrency (at the cost of more file descriptors and
    /// SQLite write contention).
    pub db_pool_size: u32,
    /// Comma-separated list of IP addresses that are blocked outright.
    ///
    /// Connections from these IPs are dropped in the accept loop before any
    /// HTTP processing.  Set via `BLOCKED_IPS` (e.g. `"1.2.3.4,5.6.7.8"`).
    /// Empty by default (no IPs blocked).
    pub blocked_ips: Vec<std::net::IpAddr>,
    /// Comma-separated allowlist of IP addresses.
    ///
    /// When non-empty, only IPs in this list are accepted.  All others are
    /// dropped in the accept loop.  Set via `ALLOWED_IPS`.  Empty by default
    /// (all IPs allowed).
    pub allowed_ips: Vec<std::net::IpAddr>,
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
    /// | `DATABASE_URL`           | `sqlite:./data.db` | SQLite database path              |
    /// | `JWT_SECRET`             | `change-me-in-production` | JWT signing secret          |
    /// | `AUTH_USERNAME`          | `admin`           | Username for /auth/token          |
    /// | `AUTH_PASSWORD`          | `secret`          | Password for /auth/token          |
    /// | `REQUEST_TIMEOUT_SECS`   | `30`              | Per-request processing timeout    |
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

        let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:./data.db".to_string());
        let jwt_secret =
            env::var("JWT_SECRET").unwrap_or_else(|_| "change-me-in-production".to_string());

        let auth_username =
            env::var("AUTH_USERNAME").unwrap_or_else(|_| "admin".to_string());
        let auth_password =
            env::var("AUTH_PASSWORD").unwrap_or_else(|_| "secret".to_string());

        let timeout_str =
            env::var("REQUEST_TIMEOUT_SECS").unwrap_or_else(|_| "30".to_string());
        let request_timeout_secs: u64 = timeout_str.parse().map_err(|_| {
            ConfigError::InvalidValue("REQUEST_TIMEOUT_SECS".into(), timeout_str.clone())
        })?;
        if request_timeout_secs == 0 {
            return Err(ConfigError::InvalidValue(
                "REQUEST_TIMEOUT_SECS".into(),
                "must be > 0".into(),
            ));
        }

        let pool_str = env::var("DB_POOL_SIZE").unwrap_or_else(|_| "5".to_string());
        let db_pool_size: u32 = pool_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("DB_POOL_SIZE".into(), pool_str.clone()))?;
        if db_pool_size == 0 {
            return Err(ConfigError::InvalidValue(
                "DB_POOL_SIZE".into(),
                "must be > 0".into(),
            ));
        }

        let blocked_ips = Self::parse_ip_list(
            &env::var("BLOCKED_IPS").unwrap_or_default(),
        );
        let allowed_ips = Self::parse_ip_list(
            &env::var("ALLOWED_IPS").unwrap_or_default(),
        );

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
            db_url,
            jwt_secret,
            auth_username,
            auth_password,
            request_timeout_secs,
            db_pool_size,
            blocked_ips,
            allowed_ips,
        })
    }

    /// Parse a comma-separated list of IP addresses, silently skipping
    /// entries that fail to parse.
    fn parse_ip_list(s: &str) -> Vec<std::net::IpAddr> {
        s.split(',')
            .filter_map(|ip| ip.trim().parse().ok())
            .collect()
    }

    /// Returns `true` if `ip` is in the explicit block-list.
    pub fn is_blocked(&self, ip: std::net::IpAddr) -> bool {
        self.blocked_ips.contains(&ip)
    }

    /// Returns `true` if `ip` is allowed (allowlist is empty ⇒ all allowed).
    pub fn is_allowed(&self, ip: std::net::IpAddr) -> bool {
        self.allowed_ips.is_empty() || self.allowed_ips.contains(&ip)
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
        assert_eq!(cfg.db_url, "sqlite:./data.db");
        assert_eq!(cfg.jwt_secret, "change-me-in-production");
        assert_eq!(cfg.auth_username, "admin");
        assert_eq!(cfg.auth_password, "secret");
        assert_eq!(cfg.request_timeout_secs, 30);
        assert_eq!(cfg.db_pool_size, 5);
        assert!(cfg.blocked_ips.is_empty());
        assert!(cfg.allowed_ips.is_empty());
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

    #[test]
    fn zero_request_timeout_secs_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("REQUEST_TIMEOUT_SECS", "0");
        let result = ServerConfig::from_env();
        env::remove_var("REQUEST_TIMEOUT_SECS");
        assert!(result.is_err());
    }

    #[test]
    fn zero_db_pool_size_returns_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("DB_POOL_SIZE", "0");
        let result = ServerConfig::from_env();
        env::remove_var("DB_POOL_SIZE");
        assert!(result.is_err());
    }

    #[test]
    fn blocked_and_allowed_ips_parsed_from_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("BLOCKED_IPS", "1.2.3.4,5.6.7.8");
        env::set_var("ALLOWED_IPS", "10.0.0.1");
        let cfg = ServerConfig::from_env().expect("config should load");
        env::remove_var("BLOCKED_IPS");
        env::remove_var("ALLOWED_IPS");
        assert_eq!(cfg.blocked_ips.len(), 2);
        assert_eq!(cfg.allowed_ips.len(), 1);
        assert!(cfg.is_blocked("1.2.3.4".parse().unwrap()));
        assert!(!cfg.is_blocked("9.9.9.9".parse().unwrap()));
        assert!(cfg.is_allowed("10.0.0.1".parse().unwrap()));
        assert!(!cfg.is_allowed("9.9.9.9".parse().unwrap()));
    }

    #[test]
    fn custom_auth_credentials_loaded_from_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("AUTH_USERNAME", "myuser");
        env::set_var("AUTH_PASSWORD", "mypass");
        let cfg = ServerConfig::from_env().expect("config should load");
        env::remove_var("AUTH_USERNAME");
        env::remove_var("AUTH_PASSWORD");
        assert_eq!(cfg.auth_username, "myuser");
        assert_eq!(cfg.auth_password, "mypass");
    }
}
