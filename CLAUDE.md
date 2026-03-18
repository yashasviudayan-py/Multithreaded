# CLAUDE.md - High-Performance Multithreaded HTTP/HTTPS Web Server

## Build & Test Commands
- Build: `cargo build`
- Run: `cargo run`
- Test all: `cargo test -- --test-threads=1`
- Single test: `cargo test <test_name>`
- Lint: `cargo clippy -- -D warnings`
- Format: `cargo fmt`
- Format check: `cargo fmt -- --check`
- Bench: `cargo bench`
- Release build: `cargo build --release`
- Security audit: `cargo audit`

## Test Suites
| File | What it covers |
|------|----------------|
| `tests/phase1_integration.rs` | TCP bind, accept loop, basic HTTP |
| `tests/phase2_integration.rs` | Router, path params, 404/405 |
| `tests/phase4_integration.rs` | Middleware, rate limiting, static files |
| `tests/phase5_integration.rs` | Concurrency limiter, keep-alive, body limits |
| `tests/phase6_integration.rs` | Edge cases, 413, connection reuse |
| `tests/phase7_integration.rs` | HTTPS, TLS handshake, HTTP→HTTPS redirect |
| `tests/phase8_integration.rs` | JWT auth, SQLite CRUD API, HTTP/2 |

**Always run integration tests with `--test-threads=1`** — parallel server instances exhaust ports and cause SIGKILL.

## Coding Standards
- **Error Handling:** Use `thiserror` for library errors (`src/server/`, `src/http/`, `src/middleware/`, `src/db/`, `src/tls/`). Use `anyhow` for application-level errors in `main.rs` only.
- **Async:** Prefer Tokio primitives. Use `tokio::spawn` for per-connection tasks. Use `tokio::task::spawn_blocking` for CPU-bound or blocking filesystem work.
- **Logging:** Use the `tracing` crate for structured logs. No `println!` in production code.
- **Documentation:** All public functions, structs, enums, and traits must have doc comments (`///`).
- **Patterns:** Use the Tower `Service` trait for middleware to stay ecosystem-compatible. Shared state goes in `AppState` (passed to `handle_connection`).
- **Naming:** Follow Rust conventions — snake_case for functions/variables, PascalCase for types/traits.
- **Modules:** Each module should have a `mod.rs` that re-exports public API items.
- **Tests:** Unit tests go in `#[cfg(test)] mod tests {}` within source files. Integration tests go in `tests/`.
- **Security:** Do not add sqlx features beyond `runtime-tokio`, `sqlite`, `derive` — adding `macros` or omitting `default-features = false` pulls in `sqlx-mysql` → `rsa` (RUSTSEC-2023-0071).

## Architecture Overview
```
src/
├── main.rs              # Entry point: Tokio runtime init, server bootstrap
├── lib.rs               # Crate root: re-exports public modules
├── server/
│   ├── mod.rs           # Server struct, TCP accept loop, graceful shutdown
│   │                    #   init_pool + JwtSecret constructed here; passed as AppState
│   └── connection.rs    # handle_connection<S> (generic: TCP + TLS), AppState,
│                        #   build_router (all application routes defined here)
├── http/
│   ├── mod.rs           # Re-exports request/response types
│   ├── request.rs       # HttpRequest: method, path, query, headers, body, path_params
│   ├── response.rs      # HttpResponse type alias + ResponseBuilder fluent API
│   └── router.rs        # Router: :param + *wildcard matching, dispatch(), 404/405
├── middleware/
│   ├── mod.rs           # Re-exports all middleware + auth helpers
│   ├── auth.rs          # JwtSecret (HS256), Claims, extract_bearer()
│   ├── logging.rs       # LoggingLayer + LoggingService<S> (Tower)
│   ├── concurrency.rs   # ConcurrencyLimiterLayer — global 503 backpressure
│   └── rate_limiter.rs  # TokenBucket, RateLimiter (DashMap), RateLimiterLayer
├── db/
│   ├── mod.rs           # Re-exports pool + models
│   ├── pool.rs          # init_pool(url) → SqlitePool, idempotent schema migration
│   └── models.rs        # Item (FromRow), CreateItem, list/get/create/delete helpers
├── tls/
│   ├── mod.rs           # Re-exports TLS helpers
│   └── acceptor.rs      # load_tls_acceptor(cert, key) → TlsAcceptor
│                        #   ALPN: ["h2", "http/1.1"] for HTTP/2 negotiation
└── static_files/
    ├── mod.rs           # serve_file(base, path) — streaming, path-traversal safe
    └── mime.rs          # MIME type detection from file extension
```

## Middleware Stack (per connection)
```
LoggingLayer            ← outermost: measures full latency, logs status + method + path
  └─ RateLimiterLayer         per-IP token bucket; 429 on exhaustion
       └─ ConcurrencyLimiterLayer  global semaphore; 503 when full
            └─ service_fn          collect body → enforce size limit → Router::dispatch
```

## Environment Variables
| Variable                  | Default                    | Description                              |
|---------------------------|----------------------------|------------------------------------------|
| `HOST`                    | `0.0.0.0`                  | Bind address                             |
| `PORT`                    | `8080`                     | Bind port                                |
| `WORKERS`                 | CPU count                  | Tokio worker thread count                |
| `BLOCKING_THREADS`        | `512`                      | Tokio blocking thread pool size          |
| `LOG_LEVEL`               | `info`                     | Tracing log filter                       |
| `STATIC_DIR`              | `./static`                 | Static files directory                   |
| `RATE_LIMIT_RPS`          | `100`                      | Requests/sec per IP                      |
| `MAX_CONNECTIONS`         | `10000`                    | Max concurrent TCP connections           |
| `MAX_BODY_BYTES`          | `4194304`                  | Max request body size (bytes)            |
| `KEEP_ALIVE_TIMEOUT`      | `75`                       | Idle keep-alive timeout (seconds)        |
| `MAX_CONCURRENT_REQUESTS` | `5000`                     | Max in-flight requests server-wide       |
| `SHUTDOWN_DRAIN_SECS`     | `30`                       | Graceful-shutdown drain timeout (seconds)|
| `TLS_CERT_PATH`           | —                          | TLS certificate PEM path                 |
| `TLS_KEY_PATH`            | —                          | TLS private key PEM path                 |
| `HTTP_REDIRECT_PORT`      | —                          | Port for HTTP→HTTPS redirect listener    |
| `DATABASE_URL`            | `sqlite:./data.db`         | SQLite database path                     |
| `JWT_SECRET`              | `change-me-in-production`  | JWT HMAC-SHA256 signing secret           |

## API Routes
| Method   | Path                    | Auth   | Status | Description                     |
|----------|-------------------------|--------|--------|---------------------------------|
| GET      | `/`                     | —      | 200    | Server name banner              |
| GET      | `/health`               | —      | 200    | Health check (`ok\n`)           |
| GET      | `/echo/:message`        | —      | 200    | Echo path parameter             |
| GET      | `/fib/:n`               | —      | 200    | n-th Fibonacci (capped at 50)   |
| GET      | `/static/*filepath`     | —      | 200    | Static file serving             |
| POST     | `/auth/token`           | —      | 200    | Issue JWT (1 hr) for credentials|
| GET      | `/api/items`            | —      | 200    | List all items (JSON array)     |
| GET      | `/api/items/:id`        | —      | 200/404| Get single item by UUID         |
| POST     | `/api/admin/items`      | Bearer | 201    | Create item `{name, description}`|
| DELETE   | `/api/admin/items/:id`  | Bearer | 200/404| Delete item by UUID             |

Default credentials for `/auth/token`: `{"username": "admin", "password": "secret"}`.

## Key Design Decisions
- **Protocol:** `hyper_util::server::conn::auto::Builder` — auto-selects HTTP/1.1 or HTTP/2 via ALPN negotiation during TLS handshake; plain connections default to HTTP/1.1.
- **TLS:** `rustls` 0.23 + `tokio-rustls` 0.26 (pure Rust, no OpenSSL). `ring` provider set explicitly via `builder_with_provider` to avoid ambiguity when multiple crypto providers are in the binary.
- **Auth:** Stateless JWT (HMAC-SHA256) via `jsonwebtoken`. `JwtSecret` wraps encode + decode keys; created once at startup and shared via `Arc<AppState>`.
- **Database:** SQLite via `sqlx` 0.8 (`default-features = false` to avoid pulling in MySQL backend and its `rsa` vulnerability). Pool size 5; schema migration runs idempotently on every start.
- **Shutdown:** `watch::channel(bool)` broadcasts shutdown to all connections; `Arc<AtomicUsize>` in-flight counter + `Notify` gates the drain timeout.
- **Connection limit:** `Semaphore::try_acquire_owned()` (non-blocking) — drops socket immediately at cap rather than queuing (DoS mitigation).
- **Rate limiter eviction:** `DashMap::retain` every 10 000 calls removes buckets idle > 5 min; early-uptime guard prevents evicting all buckets before the TTL can elapse.

## CI Pipeline (`.github/workflows/ci.yml`)
| Job            | Tool                        | Gate               |
|----------------|-----------------------------|--------------------|
| Build & Test   | `cargo build` + `cargo test`| Compile + all tests|
| Clippy         | `cargo clippy -- -D warnings`| No warnings        |
| Format         | `cargo fmt -- --check`      | Consistent style   |
| Security Audit | `cargo audit`               | No vulnerabilities |

The audit job runs `cargo audit` directly (no GitHub API calls) so no special token permissions are required.

## Phases
1. **Foundation & Networking** — Tokio runtime, TCP listener, connection management ✓
2. **HTTP Parser** — Request/response structs, zero-copy parsing, state machine ✓
3. **Thread Pool & Async Executor** — Task distribution, blocking pool, spawn_blocking ✓
4. **Middleware & Features** — Static files, structured logging, rate limiting ✓
5. **Optimization** — Memory pooling (Bytes), keep-alive, backpressure ✓
6. **Testing & Benchmarking** — Unit tests, integration tests, Criterion benchmarks ✓
7. **HTTPS / TLS** — `rustls`, self-signed certs for dev, HTTP→HTTPS redirect ✓
8. **HTTP/2 + JWT Auth + SQLite CRUD** — `auto::Builder`, JWT middleware, sqlx CRUD API ✓
