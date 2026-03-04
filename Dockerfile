FROM rust:slim-bookworm AS builder
WORKDIR /app

# Install build dependencies
RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev protobuf-compiler libclang-dev build-essential && \
    rm -rf /var/lib/apt/lists/*

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
