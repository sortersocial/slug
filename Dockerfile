# time 0.3.47+ requires Rust 1.88 (edition 2024)
FROM rust:1.88-slim as builder

WORKDIR /build

# Copy source and build. (Keep it simple to avoid remote build cache oddities.)
COPY . .
RUN cargo build --release --package slugsocial-server

FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /build/target/release/slugsocial-server /app/slugsocial-server

# Create data directory for persistent volume
RUN mkdir -p /data

ENV SLUG_DATA_DIR=/data
ENV SLUG_EVENT_LOG=/data/events.jsonl
ENV PORT=8080

EXPOSE 8080

CMD ["/app/slugsocial-server"]

