# Stage 1: Builder
FROM rust:1.94-slim AS builder

WORKDIR /app

# Cache dependency compilation separately from source
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -f target/release/deps/rust_highperf_server*

# Build the actual source
COPY . .
RUN cargo build --release

# Stage 2: Minimal runtime image
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/rust-highperf-server .
COPY --from=builder /app/static ./static

EXPOSE 8080

ENV HOST=0.0.0.0
ENV PORT=8080
ENV LOG_LEVEL=info
ENV STATIC_DIR=./static
ENV RATE_LIMIT_RPS=100
ENV MAX_CONNECTIONS=10000

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:${PORT}/health || exit 1

ENTRYPOINT ["./rust-highperf-server"]
