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
- **SQLite & PostgreSQL** — feature-selected `sqlx` pool; idempotent schema migration on startup
- **HTML templating** — Jinja2-like Tera templates compiled once at startup
- **Browser security** — database-backed sessions, Argon2id password hashes, CSRF-protected HTML forms, HttpOnly + SameSite=Strict cookies
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
# Open http://localhost:8080 for the interactive overview and demo.
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

### Run with PostgreSQL

```bash
DATABASE_URL=postgres://user:pass@localhost/mydb cargo run --features postgres
```

For the supplied containerized PostgreSQL stack, generate a bootstrap hash and
JWT secret before starting it:

```bash
export AUTH_PASSWORD_HASH="$(cargo run --quiet --bin password_hash -- 'use-a-strong-password')"
export JWT_SECRET="$(openssl rand -hex 32)"
docker compose up --build
```

---

## API Reference

| Method   | Path                    | Auth   | Description                             |
|----------|-------------------------|--------|-----------------------------------------|
| `GET`    | `/`                     | —      | Product overview / UI entry point       |
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
| `POST`   | `/ui/logout`            | Cookie + CSRF | Session logout                    |

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
| `AUTH_PASSWORD_HASH`       | —                         | Preferred Argon2id bootstrap hash; avoids a plaintext production password |
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

# Lint every target and feature combination
cargo clippy --all-targets --all-features -- -D warnings

# Format
cargo fmt

# Benchmarks
cargo bench

# Security audit
cargo audit

# PostgreSQL backend
cargo build --features postgres
```

### Browser demo

```bash
open http://localhost:8080/
```

`/ui/index` explains the request flow and where the server fits. `/ui/items`
is an interactive, session-authenticated demonstration of the database and
CSRF protection; `/ui/metrics` presents the same counters as `/metrics`.
Development credentials are `admin` / `secret`. For production, use
`AUTH_PASSWORD_HASH`, generated with:

```bash
cargo run --quiet --bin password_hash -- 'choose-a-password'
```

---

## When to use it

This project is a good foundation for services where you want a small Rust
deployment with control over the HTTP boundary rather than a full application
framework.

- **Internal APIs and admin tools** — CRUD backends, operational dashboards,
  approval workflows, or inventory services. Use SQLite for a single-node
  deployment and PostgreSQL when the service needs shared, durable state.
- **API edge or reverse proxy** — terminate TLS, enforce an IP allowlist,
  rate-limit clients, cap in-flight work, export metrics, then forward
  unmatched paths with `PROXY_UPSTREAM`.
- **Small focused services** — a narrowly scoped SaaS backend, webhook
  receiver, device gateway, or microservice where HTTP/2, structured logs,
  Prometheus metrics, and graceful shutdown are useful from day one.
- **Learning and portfolio work** — the code is intentionally readable as a
  reference for Tokio, Hyper, Tower, rustls, SQLx, sessions, and middleware.

## When not to use it yet

Do not treat this repository as a drop-in replacement for a mature API gateway
or a complete application platform without further work.

- **Public, high-scale consumer systems** need workload-specific load tests,
  independent security review, alerting, backup/restore drills, and deployment
  automation before production rollout.
- **Complex identity requirements** such as OAuth/OIDC, SSO, MFA, password
  resets, and account recovery are better handled by an identity provider or a
  dedicated authentication service.
- **Frontend-heavy products** should use a dedicated frontend application
  (for example React, Next.js, or a mobile client). The included HTML UI is an
  admin/demo surface, not a complete design system.
- **Large multi-service routing estates** often benefit from a specialized
  gateway/service mesh for traffic policy, retries, discovery, and tracing.

## Tune it for your use case

Start from the defaults, measure with a representative workload, and adjust
one limit at a time. The examples below are starting points—not universal
production values.

| Use case | Suggested starting configuration | Notes |
|----------|----------------------------------|-------|
| Internal admin tool | `DATABASE_URL=sqlite:./data.db`, `DB_POOL_SIZE=1`, `RATE_LIMIT_RPS=30`, `MAX_CONCURRENT_REQUESTS=100` | SQLite is simple for one instance; restrict access with `ALLOWED_IPS` or a private network. |
| Public JSON API | PostgreSQL build, `RATE_LIMIT_RPS=100`, `MAX_BODY_BYTES=1048576`, `MAX_CONCURRENT_REQUESTS=1000` | Set a unique `JWT_SECRET` and `AUTH_PASSWORD_HASH`; tune limits from observed p95 latency and database capacity. |
| Reverse proxy | `PROXY_UPSTREAM=https://backend.internal`, `REQUEST_TIMEOUT_SECS=15`, `MAX_BODY_BYTES=1048576` | Keep local health/metrics routes; set timeouts below the upstream’s own timeout. |
| CPU-heavy handlers | `WORKERS=<CPU cores>`, `BLOCKING_THREADS=<bounded workload limit>`, `MAX_CONCURRENT_REQUESTS=<safe CPU queue>` | CPU work uses the blocking pool. Keep its size bounded so expensive work cannot exhaust host resources. |
| High-throughput read service | `WORKERS=<CPU cores>`, `RATE_LIMIT_RPS=<measured client budget>`, `MAX_CONNECTIONS=<file-descriptor budget>` | Benchmark release builds with `wrk` or `hey`; raise connection and request limits only after confirming memory, database, and downstream headroom. |

Before every production deployment:

1. Use TLS and set `TLS_CERT_PATH`/`TLS_KEY_PATH`; the UI session cookie then
   receives the `Secure` attribute automatically.
2. Set `JWT_SECRET` to a long, random value and bootstrap the administrator
   with `AUTH_PASSWORD_HASH`, never the development defaults.
3. Run `cargo test -- --test-threads=1`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo audit`.
4. Monitor `/metrics`, set alerts for `5xx`, timeouts, rate limits, and
   concurrency rejections, and load-test the exact deployment shape.

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
│   ├── pool.rs          # SQLite/PostgreSQL pool, schema migration
│   └── models.rs        # Item CRUD + Argon2id user helpers
├── tls/
│   └── acceptor.rs      # load_tls_acceptor(), ALPN h2/http1.1
├── metrics.rs           # AtomicU64 counters, Prometheus text render
├── session/mod.rs       # Shared database SessionStore (UUID + CSRF tokens)
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
| Build & Test   | all feature builds + every integration suite |
| Lint           | `cargo clippy --all-targets --all-features -- -D warnings` |
| Format         | `cargo fmt -- --check`        |
| Security Audit | `cargo audit`                 |

---

## License

MIT
