# CLAUDE.md - High-Performance Multithreaded HTTP Web Server

## Build & Test Commands
- Build: `cargo build`
- Run: `cargo run`
- Test: `cargo test`
- Single test: `cargo test <test_name>`
- Lint: `cargo clippy -- -D warnings`
- Format: `cargo fmt`
- Format check: `cargo fmt -- --check`
- Bench: `cargo bench`
- Release build: `cargo build --release`

## Coding Standards
- **Error Handling:** Use `thiserror` for library errors (src/server/, src/http/, src/middleware/). Use `anyhow` for application-level errors in `main.rs` only.
- **Async:** Prefer Tokio primitives. Use `tokio::spawn` for per-connection tasks. Use `tokio::task::spawn_blocking` for CPU-bound or blocking filesystem work.
- **Logging:** Use the `tracing` crate for structured logs. No `println!` in production code.
- **Documentation:** All public functions, structs, enums, and traits must have doc comments (`///`).
- **Patterns:** Use the Tower `Service` trait for middleware (rate limiter, logging) to stay ecosystem-compatible.
- **Naming:** Follow Rust conventions — snake_case for functions/variables, PascalCase for types/traits.
- **Modules:** Each module should have a `mod.rs` that re-exports public API items.
- **Tests:** Unit tests go in `#[cfg(test)] mod tests {}` within source files. Integration tests go in `tests/`.

## Architecture Overview
```
src/
├── main.rs              # Entry point: Tokio runtime init, server bootstrap
├── lib.rs               # Crate root: re-exports public modules
├── server/
│   ├── mod.rs           # Server struct, TCP listener, connection accept loop
│   └── connection.rs    # Per-connection handling, task spawning
├── http/
│   ├── mod.rs           # Re-exports request/response types
│   ├── request.rs       # HTTP request parsing (zero-copy with httparse)
│   ├── response.rs      # HTTP response building and serialization
│   └── router.rs        # Route matching and handler dispatch
├── middleware/
│   ├── mod.rs           # Middleware stack composition
│   ├── logging.rs       # Structured request/response logging via tracing
│   └── rate_limiter.rs  # Token-bucket rate limiter (per-IP)
└── static_files/
    ├── mod.rs           # Static file serving with async I/O
    └── mime.rs          # MIME type detection
```

## Project Context
Building a high-performance HTTP server targeting 50k+ req/sec. Performance and memory safety are top priorities. Key design decisions:
- Tokio multi-threaded runtime (worker threads = CPU cores)
- Hyper for HTTP/1.1 protocol handling
- Tower Service trait for composable middleware
- Zero-copy parsing where possible (httparse, Bytes)
- Token-bucket rate limiting per client IP
- Async file I/O with streaming (no full-file buffering)

## Phases
1. **Foundation & Networking** — Tokio runtime, TCP listener, connection management
2. **HTTP Parser** — Request/response structs, zero-copy parsing, state machine
3. **Thread Pool & Async Executor** — Task distribution, blocking pool, spawn_blocking
4. **Middleware & Features** — Static files, structured logging, rate limiting
5. **Optimization** — Memory pooling (Bytes), keep-alive, backpressure
6. **Testing & Benchmarking** — Unit tests, integration tests, wrk/hey load testing
