use anyhow::{Context, Result};
use tracing::info;
use tracing_subscriber::EnvFilter;

use rust_highperf_server::config::ServerConfig;
use rust_highperf_server::server::Server;

fn main() -> Result<()> {
    let config = ServerConfig::from_env().context("Failed to load server config")?;

    // Initialise tracing before building the runtime so that any runtime
    // construction errors are visible in the log output.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level)),
        )
        .init();

    info!(
        addr           = %config.addr,
        workers        = config.workers,
        blocking_threads = config.max_blocking_threads,
        log_level      = %config.log_level,
        "rust-highperf-server starting up"
    );

    // Build a fully-configured multi-thread runtime instead of relying on the
    // `#[tokio::main]` macro defaults.  This lets us wire in ServerConfig
    // values (worker count, blocking pool size, thread names) at startup.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.workers)
        .max_blocking_threads(config.max_blocking_threads)
        .thread_name("tokio-worker")
        .enable_all()
        .build()
        .context("Failed to build Tokio runtime")?;

    runtime.block_on(async {
        Server::new(config).run().await?;
        Ok::<(), anyhow::Error>(())
    })?;

    // The tracing subscriber is synchronous (writes to stdout), so this log
    // line is safe after block_on returns (runtime is still alive until drop).
    info!("Server shut down cleanly");
    Ok(())
}
