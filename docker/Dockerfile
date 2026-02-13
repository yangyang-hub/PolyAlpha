# Build stage
FROM rust:1.88-bookworm AS builder

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY crates/pa-core/Cargo.toml crates/pa-core/Cargo.toml
COPY crates/pa-market-data/Cargo.toml crates/pa-market-data/Cargo.toml
COPY crates/pa-strategy/Cargo.toml crates/pa-strategy/Cargo.toml
COPY crates/pa-execution/Cargo.toml crates/pa-execution/Cargo.toml
COPY crates/pa-risk/Cargo.toml crates/pa-risk/Cargo.toml
COPY crates/pa-storage/Cargo.toml crates/pa-storage/Cargo.toml
COPY crates/pa-backtest/Cargo.toml crates/pa-backtest/Cargo.toml
COPY crates/pa-monitor/Cargo.toml crates/pa-monitor/Cargo.toml

# Create dummy source files for dependency caching
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs
RUN for crate in pa-core pa-market-data pa-strategy pa-execution pa-risk pa-storage pa-backtest pa-monitor; do \
    mkdir -p crates/$crate/src && echo '' > crates/$crate/src/lib.rs; \
    done

# Build dependencies only (cached layer)
RUN cargo build --release 2>/dev/null || true

# Copy actual source code
COPY . .

# Touch source files to invalidate the cache for our code only
RUN find src crates -name "*.rs" -exec touch {} +

# Test stage (used by CI: docker build --target test)
FROM builder AS test
RUN cargo test --workspace

# Build the real binary
FROM builder AS release
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=release /app/target/release/polyalpha /app/polyalpha
COPY config/ /app/config/

ENV RUST_LOG=info
ENV RUN_MODE=production

EXPOSE 8080 9090

CMD ["/app/polyalpha"]
