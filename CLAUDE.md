# CLAUDE.md - High-Performance Multithreaded HTTP/HTTPS Web Server

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
│   └── connection.rs    # Per-connection handling, AppState, build_router
├── http/
│   ├── mod.rs           # Re-exports request/response types
│   ├── request.rs       # HTTP request parsing (zero-copy with httparse)
│   ├── response.rs      # HTTP response building and serialization
│   └── router.rs        # Route matching and handler dispatch
├── middleware/
│   ├── mod.rs           # Middleware stack composition
│   ├── auth.rs          # JWT auth: JwtSecret, Claims, extract_bearer
│   ├── logging.rs       # Structured request/response logging via tracing
│   ├── concurrency.rs   # ConcurrencyLimiterLayer (global 503 backpressure)
│   └── rate_limiter.rs  # Token-bucket rate limiter (per-IP)
├── db/
│   ├── mod.rs           # Re-exports pool + models
│   ├── pool.rs          # SQLite pool init, schema migration
│   └── models.rs        # Item struct, CRUD helpers (sqlx)
├── tls/
│   ├── mod.rs           # Re-exports TLS helpers
│   └── acceptor.rs      # TlsAcceptor from PEM cert+key, ALPN (h2, http/1.1)
└── static_files/
    ├── mod.rs           # Static file serving with async I/O
    └── mime.rs          # MIME type detection
```

## Environment Variables (Phase 8 additions)
| Variable       | Default                   | Description                        |
|----------------|---------------------------|------------------------------------|
| `DATABASE_URL` | `sqlite:./data.db`        | SQLite database path               |
| `JWT_SECRET`   | `change-me-in-production` | JWT HMAC-SHA256 signing secret     |

## API Routes (Phase 8)
| Method | Path                    | Auth     | Description                    |
|--------|-------------------------|----------|--------------------------------|
| POST   | `/auth/token`           | None     | Issue JWT for valid credentials|
| GET    | `/api/items`            | None     | List all items                 |
| GET    | `/api/items/:id`        | None     | Get single item by ID          |
| POST   | `/api/admin/items`      | Bearer   | Create a new item (201)        |
| DELETE | `/api/admin/items/:id`  | Bearer   | Delete an item                 |

Default credentials: `{"username": "admin", "password": "secret"}` (override in production).

## Project Context
Building a production-ready, high-performance HTTP/HTTPS server targeting 50k+ req/sec. Performance and memory safety are top priorities. Key design decisions:
- Tokio multi-threaded runtime (worker threads = CPU cores)
- Hyper for HTTP/1.1 + HTTP/2 protocol handling (`auto::Builder` with ALPN)
- Tower Service trait for composable middleware
- Zero-copy parsing where possible (httparse, Bytes)
- Token-bucket rate limiting per client IP
- Async file I/O with streaming (no full-file buffering)
- **TLS strategy:** HTTPS via `rustls` (pure Rust, no OpenSSL). ALPN advertises `h2` + `http/1.1` so HTTP/2 is negotiated automatically over TLS.
- **Auth strategy:** Stateless JWT (HMAC-SHA256) via `jsonwebtoken`. `JwtSecret` created at startup and shared via `Arc<AppState>`.
- **Database strategy:** SQLite via `sqlx` (async, compile-time-free queries). Pool created once at startup; schema is migrated idempotently on every start.

## Phases
1. **Foundation & Networking** — Tokio runtime, TCP listener, connection management ✓
2. **HTTP Parser** — Request/response structs, zero-copy parsing, state machine ✓
3. **Thread Pool & Async Executor** — Task distribution, blocking pool, spawn_blocking ✓
4. **Middleware & Features** — Static files, structured logging, rate limiting ✓
5. **Optimization** — Memory pooling (Bytes), keep-alive, backpressure ✓
6. **Testing & Benchmarking** — Unit tests, integration tests, wrk/hey load testing ✓
7. **HTTPS / TLS** — `rustls`, self-signed certs for dev, HTTP→HTTPS redirect ✓
8. **HTTP/2 + JWT Auth + SQLite CRUD** — `auto::Builder`, JWT middleware, sqlx CRUD API ✓
