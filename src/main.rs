use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

use rust_highperf_server::config::ServerConfig;

#[tokio::main]
async fn main() -> Result<()> {
    let config = ServerConfig::from_env()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level)),
        )
        .init();

    info!(
        addr = %config.addr,
        workers = config.workers,
        log_level = %config.log_level,
        "rust-highperf-server starting up"
    );

    // Server bootstrap will be implemented in Phase 1
    Ok(())
}
