//! Criterion micro-benchmarks for the high-performance HTTP server.
//!
//! Run with: `cargo bench`
//! HTML reports are written to `target/criterion/`.
//!
//! Benchmark groups:
//! 1. **request** — query-param lookup, percent-decode, `HttpRequest` construction
//! 2. **router**  — literal match, param match, miss (full scan)
//! 3. **response** — small-body, medium-body, `full_body()` boxing overhead
//! 4. **rate_limiter** — token-bucket check (allowed, saturated, 100-IP fan-out)

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use hyper::Method;
use rust_highperf_server::http::request::HttpRequest;
use rust_highperf_server::http::response::{full_body, ResponseBuilder};
use rust_highperf_server::http::router::Router;
use rust_highperf_server::middleware::rate_limiter::RateLimiter;
use std::net::IpAddr;
use std::str::FromStr as _;
use std::sync::Arc;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_request(method: Method, uri: &str) -> HttpRequest {
    let (parts, _) = hyper::Request::builder()
        .method(method)
        .uri(uri)
        .body(())
        .unwrap()
        .into_parts();
    HttpRequest::from_parts(parts, Bytes::new())
}

fn build_router() -> Router {
    let mut r = Router::new();
    r.get("/", |_req| async { ResponseBuilder::ok().text("root\n") });
    r.get("/health", |_req| async {
        ResponseBuilder::ok().text("ok\n")
    });
    r.get("/echo/:msg", |req| async move {
        let msg = req.path_param("msg").unwrap_or("").to_string();
        ResponseBuilder::ok().text(format!("{msg}\n"))
    });
    r.get("/users/:uid/posts/:pid", |req| async move {
        let uid = req.path_param("uid").unwrap_or("?").to_string();
        let pid = req.path_param("pid").unwrap_or("?").to_string();
        ResponseBuilder::ok().text(format!("{uid}/{pid}\n"))
    });
    r
}

// ── Group 1: Request parsing / accessors ─────────────────────────────────────

fn bench_request(c: &mut Criterion) {
    let mut g = c.benchmark_group("request");

    // query_param: linear scan over 5 parameters, key is last
    g.bench_function("query_param_5_pairs_last", |b| {
        let req = make_request(
            Method::GET,
            "/search?a=1&b=2&c=hello%20world&d=foo%2Bbar&name=rustacean",
        );
        b.iter(|| {
            std::hint::black_box(req.query_param("name"));
        });
    });

    // query_param: key absent (full scan, returns None)
    g.bench_function("query_param_miss", |b| {
        let req = make_request(Method::GET, "/search?a=1&b=2&c=3");
        b.iter(|| {
            std::hint::black_box(req.query_param("missing"));
        });
    });

    // HttpRequest construction cost (allocation + header map clone)
    g.bench_function("from_parts_with_body", |b| {
        b.iter_batched(
            || {
                hyper::Request::builder()
                    .method(Method::POST)
                    .uri("/data?key=value")
                    .header("content-type", "application/json")
                    .body(())
                    .unwrap()
                    .into_parts()
            },
            |(parts, _)| {
                std::hint::black_box(HttpRequest::from_parts(
                    parts,
                    Bytes::from_static(b"{\"hello\":\"world\"}"),
                ))
            },
            BatchSize::SmallInput,
        );
    });

    g.finish();
}

// ── Group 2: Router dispatch ──────────────────────────────────────────────────

fn bench_router(c: &mut Criterion) {
    let router = Arc::new(build_router());
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut g = c.benchmark_group("router");

    // Literal match: /health is the second registered route — one comparison miss
    g.bench_function("literal_hit", |b| {
        b.iter_batched(
            || make_request(Method::GET, "/health"),
            |req| rt.block_on(router.dispatch(req)),
            BatchSize::SmallInput,
        );
    });

    // Param match: /users/:uid/posts/:pid — two captures
    g.bench_function("two_param_hit", |b| {
        b.iter_batched(
            || make_request(Method::GET, "/users/42/posts/7"),
            |req| rt.block_on(router.dispatch(req)),
            BatchSize::SmallInput,
        );
    });

    // Full miss: path not registered — scans all routes, returns 404
    g.bench_function("miss_404", |b| {
        b.iter_batched(
            || make_request(Method::GET, "/no-such-route-at-all"),
            |req| rt.block_on(router.dispatch(req)),
            BatchSize::SmallInput,
        );
    });

    g.finish();
}

// ── Group 3: Response building ────────────────────────────────────────────────

fn bench_response(c: &mut Criterion) {
    let mut g = c.benchmark_group("response");

    // Small plain-text body (~14 bytes)
    g.bench_function("small_text", |b| {
        b.iter(|| std::hint::black_box(ResponseBuilder::ok().text("Hello, world!\n")));
    });

    // Medium JSON body (~200 bytes); serde_json is not a dep so we use a raw string
    g.bench_function("medium_json_string", |b| {
        let body = Bytes::from_static(
            br#"{"status":"ok","code":200,"message":"everything is fine","ts":1700000000}"#,
        );
        b.iter(|| {
            std::hint::black_box(ResponseBuilder::ok().bytes_body("application/json", body.clone()))
        });
    });

    // full_body() boxing cost for a 64 KiB payload
    g.bench_function("full_body_64k", |b| {
        let payload = Bytes::from(vec![b'x'; 65_536]);
        b.iter(|| std::hint::black_box(full_body(payload.clone())));
    });

    // empty response (no body allocation)
    g.bench_function("empty", |b| {
        b.iter(|| std::hint::black_box(ResponseBuilder::ok().empty()));
    });

    g.finish();
}

// ── Group 4: Rate-limiter token-bucket ───────────────────────────────────────

fn bench_rate_limiter(c: &mut Criterion) {
    let mut g = c.benchmark_group("rate_limiter");

    let ip_local: IpAddr = IpAddr::from_str("127.0.0.1").unwrap();

    // Happy path: bucket never exhausted (1 000 000 rps limit)
    g.bench_function("check_allowed", |b| {
        let limiter = RateLimiter::new(1_000_000);
        b.iter(|| std::hint::black_box(limiter.check(ip_local)));
    });

    // Saturated bucket: rate = 1, pre-exhaust the bucket first
    g.bench_function("check_saturated", |b| {
        let limiter = RateLimiter::new(1);
        // Exhaust the single token before the benchmark loop starts.
        limiter.check(ip_local);
        b.iter(|| std::hint::black_box(limiter.check(ip_local)));
    });

    // 100 distinct IPs round-robin — exercises DashMap sharding
    g.bench_function("check_100_ips", |b| {
        let limiter = RateLimiter::new(1_000_000);
        let ips: Vec<IpAddr> = (0u8..100)
            .map(|i| IpAddr::from_str(&format!("10.0.0.{i}")).unwrap())
            .collect();
        let mut idx = 0usize;
        b.iter(|| {
            let result = limiter.check(ips[idx % 100]);
            idx = idx.wrapping_add(1);
            std::hint::black_box(result)
        });
    });

    // Parameterized: vary rps limit to show arithmetic cost delta
    for rps in [100u32, 10_000, 1_000_000] {
        g.bench_with_input(BenchmarkId::new("check_rps_limit", rps), &rps, |b, &rps| {
            let limiter = RateLimiter::new(rps);
            b.iter(|| std::hint::black_box(limiter.check(ip_local)));
        });
    }

    g.finish();
}

// ── Entry points ──────────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_request,
    bench_router,
    bench_response,
    bench_rate_limiter,
);
criterion_main!(benches);
