# Multithreaded

[![CI](https://github.com/yashasviudayan-py/Multithreaded/actions/workflows/ci.yml/badge.svg)](https://github.com/yashasviudayan-py/Multithreaded/actions/workflows/ci.yml)
[![Security Audit](https://github.com/yashasviudayan-py/Multithreaded/actions/workflows/ci.yml/badge.svg?label=audit)](https://github.com/yashasviudayan-py/Multithreaded/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A production-ready, high-performance HTTP/HTTPS web server built from scratch in Rust. Implements HTTP/1.1 and HTTP/2, TLS via `rustls`, JWT authentication, SQLite/PostgreSQL persistence, Tera HTML templating, cookie-based sessions, a Prometheus-compatible metrics endpoint, and an HTTP reverse proxy — all on top of Tokio and Hyper.

---

## Features

- **HTTP/1.1 & HTTP/2** — automatic protocol selection via ALPN during TLS handshake
- **TLS / HTTPS** — pure-Rust `rustls` (no OpenSSL); HTTP → HTTPS redirect listener
- **Tower middleware stack** — structured logging, per-IP rate limiting (token bucket), global concurrency backpressure
- **JWT authentication** — stateless HMAC-SHA256 tokens via `jsonwebtoken`
- **SQLite & PostgreSQL** — `sqlx` connection pool; idempotent schema migration on startup
- **HTML templating** — Jinja2-like Tera templates compiled once at startup
- **Cookie sessions** — in-memory server-side sessions (UUID tokens, 1-hour TTL, HttpOnly + SameSite=Strict)
- **Prometheus metrics** — lock-free atomic counters rendered at `/metrics`
- **Reverse proxy** — forward unmatched routes to an upstream server via `reqwest`
- **IP filter** — per-connection allowlist / blocklist enforced before HTTP processing
- **Static file serving** — streaming with path-traversal protection and MIME detection
- **Graceful shutdown** — drain in-flight requests before exit; configurable timeout
- **Criterion benchmarks** — micro-benchmarks for the hot path

---

## Quick Start

### Prerequisites

- Rust 1.75+ ([rustup](https://rustup.rs))
- `cargo audit` for security scanning: `cargo install cargo-audit`

### Run (HTTP)

```bash
cargo run
# Server listening on http://0.0.0.0:8080
```

### Run (HTTPS)

```bash
# Generate a self-signed cert for development
openssl req -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem -days 365 -nodes -subj '/CN=localhost'

TLS_CERT_PATH=cert.pem TLS_KEY_PATH=key.pem cargo run
# HTTPS on :8080 — HTTP→HTTPS redirect listener optional via HTTP_REDIRECT_PORT=8081
```

### Reverse Proxy Mode

```bash
PROXY_UPSTREAM=http://backend:3000 cargo run
# All requests not matched by a local route are forwarded upstream
```

---

## API Reference

| Method   | Path                    | Auth   | Description                             |
|----------|-------------------------|--------|-----------------------------------------|
| `GET`    | `/`                     | —      | Server banner                           |
| `GET`    | `/health`               | —      | Health check                            |
| `GET`    | `/echo/:message`        | —      | Echo path parameter                     |
| `GET`    | `/fib/:n`               | —      | Fibonacci(n), capped at n=50            |
| `GET`    | `/static/*filepath`     | —      | Static file serving                     |
| `POST`   | `/auth/token`           | —      | Issue JWT (1 hr)                        |
| `GET`    | `/api/items`            | —      | List all items (JSON)                   |
| `GET`    | `/api/items/:id`        | —      | Get item by UUID                        |
| `POST`   | `/api/admin/items`      | Bearer | Create item `{name, description}`       |
| `DELETE` | `/api/admin/items/:id`  | Bearer | Delete item by UUID                     |
| `GET`    | `/metrics`              | —      | Prometheus-format metrics               |
| `GET`    | `/ui`                   | Cookie | HTML dashboard                          |
| `POST`   | `/ui/login`             | —      | Session login                           |
| `POST`   | `/ui/logout`            | Cookie | Session logout                          |

#### Get a JWT

```bash
curl -s -X POST http://localhost:8080/auth/token \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"secret"}' | jq .
```

#### Create an item (requires JWT)

```bash
TOKEN=$(curl -s -X POST http://localhost:8080/auth/token \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"secret"}' | jq -r .token)

curl -s -X POST http://localhost:8080/api/admin/items \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"widget","description":"a useful thing"}' | jq .
```

---

## Configuration

All settings are loaded from environment variables with sensible defaults.

| Variable                   | Default                   | Description                               |
|----------------------------|---------------------------|-------------------------------------------|
| `HOST`                     | `0.0.0.0`                 | Bind address                              |
| `PORT`                     | `8080`                    | Bind port                                 |
| `WORKERS`                  | CPU count                 | Tokio worker threads                      |
| `BLOCKING_THREADS`         | `512`                     | Blocking thread pool size                 |
| `LOG_LEVEL`                | `info`                    | Tracing filter (`error`, `debug`, …)      |
| `STATIC_DIR`               | `./static`                | Static files root                         |
| `RATE_LIMIT_RPS`           | `100`                     | Requests/sec per client IP                |
| `MAX_CONNECTIONS`          | `10000`                   | Max concurrent TCP connections            |
| `MAX_BODY_BYTES`           | `4194304`                 | Max request body (bytes)                  |
| `KEEP_ALIVE_TIMEOUT`       | `75`                      | Idle keep-alive timeout (seconds)         |
| `MAX_CONCURRENT_REQUESTS`  | `5000`                    | Max in-flight requests server-wide        |
| `SHUTDOWN_DRAIN_SECS`      | `30`                      | Graceful-shutdown drain (seconds)         |
| `REQUEST_TIMEOUT_SECS`     | `30`                      | Per-request processing timeout (seconds)  |
| `DATABASE_URL`             | `sqlite:./data.db`        | SQLite or PostgreSQL URL                  |
| `DB_POOL_SIZE`             | `5`                       | Database connection pool size             |
| `JWT_SECRET`               | `change-me-in-production` | HMAC-SHA256 signing secret                |
| `AUTH_USERNAME`            | `admin`                   | Username for `/auth/token`                |
| `AUTH_PASSWORD`            | `secret`                  | Password for `/auth/token`                |
| `TLS_CERT_PATH`            | —                         | TLS certificate PEM path                  |
| `TLS_KEY_PATH`             | —                         | TLS private key PEM path                  |
| `HTTP_REDIRECT_PORT`       | —                         | HTTP → HTTPS redirect port                |
| `BLOCKED_IPS`              | —                         | Comma-separated IPs to block at accept    |
| `ALLOWED_IPS`              | —                         | Comma-separated IP allowlist              |
| `PROXY_UPSTREAM`           | —                         | Upstream URL for reverse-proxy mode       |
| `PROXY_STRIP_PREFIX`       | —                         | Path prefix to strip before forwarding    |

---

## Development

```bash
# Build
cargo build

# Run all tests (must be single-threaded — server instances share ports)
cargo test -- --test-threads=1

# Run a single test
cargo test test_name

# Lint
cargo clippy -- -D warnings

# Format
cargo fmt

# Benchmarks
cargo bench

# Security audit
cargo audit

# PostgreSQL backend
cargo build --features postgres
```

### With PostgreSQL

```bash
DATABASE_URL=postgres://user:pass@localhost/mydb cargo run --features postgres
```

---

## Architecture

```
src/
├── main.rs              # Tokio runtime init, server bootstrap
├── server/
│   ├── mod.rs           # TCP accept loop, graceful shutdown, AppState assembly
│   ├── connection.rs    # handle_connection<S>, build_router, Tower middleware stack
│   └── task.rs          # run_blocking() wrapper for spawn_blocking
├── http/
│   ├── request.rs       # HttpRequest parsing
│   ├── response.rs      # HttpResponse + ResponseBuilder
│   └── router.rs        # :param + *wildcard routing, 404/405
├── middleware/
│   ├── auth.rs          # JwtSecret, extract_bearer()
│   ├── logging.rs       # LoggingLayer (Tower)
│   ├── concurrency.rs   # ConcurrencyLimiterLayer — 503 at cap
│   └── rate_limiter.rs  # Token-bucket RateLimiterLayer — 429 at limit
├── db/
│   ├── pool.rs          # init_pool(), schema migration
│   └── models.rs        # Item CRUD helpers
├── tls/
│   └── acceptor.rs      # load_tls_acceptor(), ALPN h2/http1.1
├── metrics.rs           # AtomicU64 counters, Prometheus text render
├── session/mod.rs       # SessionStore (UUID tokens, DashMap, 1h TTL)
├── templates/mod.rs     # TemplateEngine (Arc<Tera>)
├── proxy/mod.rs         # proxy_request(), hop-by-hop header stripping
└── static_files/        # serve_file(), MIME detection
```

### Middleware Stack (per connection)

```
LoggingLayer              ← measures full latency, logs status + method + path
  └─ RateLimiterLayer     ← per-IP token bucket; 429 on exhaustion
       └─ ConcurrencyLimiterLayer  ← global semaphore; 503 when full
            └─ service_fn          ← body collection, size limit, Router::dispatch
```

---

## CI

Every push runs:

| Job            | Tool                          |
|----------------|-------------------------------|
| Build & Test   | `cargo build` + `cargo test`  |
| Lint           | `cargo clippy -- -D warnings` |
| Format         | `cargo fmt -- --check`        |
| Security Audit | `cargo audit`                 |

---

## License

MIT
