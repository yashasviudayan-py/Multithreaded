//! Blocking task utilities for safely offloading CPU-bound or blocking I/O
//! work off Tokio's async worker threads.
//!
//! Tokio's async worker threads drive the event loop; any blocking call on
//! them stalls every other task sharing that thread.  Use [`run_blocking`]
//! whenever a handler needs to do synchronous work (e.g., CPU-intensive
//! computation, synchronous file access, or calls into blocking C libraries).
//!
//! Under the hood this delegates to [`tokio::task::spawn_blocking`], which
//! dispatches work to a separate thread pool whose size is controlled by
//! [`ServerConfig::max_blocking_threads`](crate::config::ServerConfig::max_blocking_threads).

use thiserror::Error;

/// Errors that can occur when running a blocking task.
#[derive(Debug, Error)]
pub enum TaskError {
    /// The blocking closure panicked; the message is the stringified payload.
    #[error("Blocking task panicked: {0}")]
    Panic(String),
    /// The task was cancelled before it could complete (e.g., runtime shutdown).
    #[error("Blocking task was cancelled")]
    Cancelled,
}

/// Run a blocking or CPU-bound closure on Tokio's blocking thread pool.
///
/// This is a thin, ergonomic wrapper around [`tokio::task::spawn_blocking`]
/// that converts `JoinError` (panics, cancellations) into [`TaskError`] so
/// callers never have to `unwrap` a `JoinHandle`.
///
/// # Example
/// ```no_run
/// use rust_highperf_server::server::task::run_blocking;
///
/// async fn handler() {
///     let result = run_blocking(|| expensive_cpu_work()).await.unwrap();
/// }
/// # fn expensive_cpu_work() -> u64 { 0 }
/// ```
pub async fn run_blocking<F, R>(f: F) -> Result<R, TaskError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(join_error_to_task_error)
}

/// Convert a [`tokio::task::JoinError`] into a [`TaskError`], extracting the
/// panic message where possible.
fn join_error_to_task_error(e: tokio::task::JoinError) -> TaskError {
    if e.is_cancelled() {
        return TaskError::Cancelled;
    }
    // is_panic() is the only other variant
    let payload = e.into_panic();
    let msg = if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic payload".to_string()
    };
    TaskError::Panic(msg)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_blocking_returns_value() {
        let result = run_blocking(|| 6 * 7).await.unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn run_blocking_propagates_panic_as_error() {
        let result = run_blocking(|| -> u32 { panic!("oops") }).await;
        match result {
            Err(TaskError::Panic(msg)) => assert_eq!(msg, "oops"),
            other => panic!("expected TaskError::Panic, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_blocking_heavy_work_completes() {
        // Simulate CPU-bound work — should complete without blocking the executor.
        let result = run_blocking(|| (0u64..1_000_000).sum::<u64>())
            .await
            .unwrap();
        assert_eq!(result, 499_999_500_000);
    }
}
