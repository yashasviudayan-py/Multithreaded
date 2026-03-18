//! High-performance multithreaded HTTP web server.
//!
//! Built on Tokio and Hyper, targeting 50k+ req/sec with:
//! - Async I/O with per-connection task spawning
//! - Zero-copy HTTP parsing
//! - Token-bucket rate limiting
//! - Streaming static file serving
//! - Structured logging via tracing

pub mod config;
pub mod db;
pub mod http;
pub mod middleware;
pub mod server;
pub mod static_files;
pub mod tls;

pub use server::Server;
