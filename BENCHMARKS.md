# Benchmarks & Load Testing

## Criterion micro-benchmarks

Run the full benchmark suite:

```bash
cargo bench
```

View the HTML report (generated in `target/criterion/`):

```bash
open target/criterion/report/index.html   # macOS
xdg-open target/criterion/report/index.html  # Linux
```

Benchmark groups:

| Group | What it measures |
|-------|-----------------|
| `request/` | `query_param` scan, `percent_decode`, `HttpRequest::from_parts` allocation |
| `router/` | Literal match, two-param match, miss (full scan → 404) |
| `response/` | Small text, medium JSON, `full_body()` 64 KiB boxing, empty body |
| `rate_limiter/` | Token-bucket `check()` — allowed, saturated, 100-IP fan-out, rps sweep |

Compile benchmarks without running them (useful in CI):

```bash
cargo bench --no-run
```

---

## Load testing with `wrk`

Install: `brew install wrk` (macOS) or `apt install wrk` (Ubuntu)

Build and start the server with rate-limiting disabled for benchmarking:

```bash
cargo build --release
RATE_LIMIT_RPS=1000000 MAX_CONCURRENT_REQUESTS=50000 ./target/release/rust-highperf-server
```

Run a 30-second load test (12 threads, 400 concurrent connections):

```bash
wrk -t12 -c400 -d30s http://localhost:8080/health
wrk -t12 -c400 -d30s http://localhost:8080/echo/hello
wrk -t12 -c400 -d30s "http://localhost:8080/fib/5"
```

---

## Load testing with `hey`

Install: `go install github.com/rakyll/hey@latest`

```bash
# 100 000 requests, 200 concurrent workers
hey -n 100000 -c 200 http://localhost:8080/health
hey -n 50000  -c 100 http://localhost:8080/fib/10

# Keep-alive (default in hey) to measure throughput per connection
hey -n 100000 -c 400 -disable-keepalive=false http://localhost:8080/health
```

---

## Flamegraph profiling

Requires [cargo-flamegraph](https://github.com/flamegraph-rs/flamegraph):

```bash
cargo install flamegraph

# Profile the micro-benchmarks
sudo cargo flamegraph --bench server_bench -- --bench

# Profile the running server under wrk load
sudo cargo flamegraph --bin rust-highperf-server &
wrk -t4 -c100 -d20s http://localhost:8080/health
# flamegraph.svg generated in the current directory
```

---

## Performance targets

| Endpoint | Target (release build, loopback) |
|----------|----------------------------------|
| `GET /health` | ≥ 50 000 req/s |
| `GET /echo/:msg` | ≥ 40 000 req/s |
| `GET /fib/10` | ≥ 20 000 req/s (CPU-bound) |
| `GET /static/<small file>` | ≥ 30 000 req/s |
