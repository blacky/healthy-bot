# 1. Preparation Stage
FROM lukemathwalker/cargo-chef:latest-rust-slim-bookworm AS chef
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# 2. Builder Stage
FROM chef AS builder 
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the layer that gets cached!
RUN cargo chef cook --release --recipe-path recipe.json

# Build the actual application
COPY . .
RUN cargo build --release

# 3. Test Stage
FROM builder AS tester
# Run tests during build. If they fail, the build fails.
RUN cargo test --release

# 4. Runtime Stage
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y ca-certificates libsqlite3-0 && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# Ensure we have the data directories the app expects
RUN mkdir -p /healthybot/db /healthybot/data/markovs

COPY --from=builder /app/target/release/healthy-bot /app/healthy-bot

# Set production log level if not provided
ENV RUST_LOG=info

CMD ["/app/healthy-bot"]
