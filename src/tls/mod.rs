//! TLS support for Phase 7: HTTPS via rustls.
//!
//! This module loads TLS certificates and private keys from PEM files and
//! creates a [`tokio_rustls::TlsAcceptor`] that can be used to upgrade plain
//! TCP connections to TLS in the server accept loop.
//!
//! ## Design decisions
//! - Pure-Rust TLS via `rustls` — no OpenSSL dependency, memory-safe.
//! - `tokio-rustls` provides the async wrapper around rustls for use with Tokio.
//! - Certificate loading is done once at startup; errors surface before the
//!   server starts accepting connections.

pub mod acceptor;

pub use acceptor::{load_tls_acceptor, TlsError};
