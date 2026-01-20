FROM lukemathwalker/cargo-chef:latest-rust-1-slim AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

# Install build dependencies
RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev protobuf-compiler libclang-dev build-essential && \
    rm -rf /var/lib/apt/lists/*

# Build dependencies (cached layer)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Build application
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim

WORKDIR /app

RUN apt-get update && \
    apt-get install -y ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/kaswallet-daemon /app/
COPY --from=builder /app/target/release/kaswallet-create /app/
COPY --from=builder /app/target/release/kaswallet-cli /app/
COPY --from=builder /app/target/release/kaswallet-dump-mnemonics /app/
COPY --from=builder /app/target/release/kaswallet-test-client /app/

EXPOSE 8082

ENTRYPOINT ["/app/kaswallet-daemon"]
