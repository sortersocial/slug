FROM rust:1.75-slim as builder

WORKDIR /build

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY server/Cargo.toml ./server/

# Create a dummy source to build dependencies
RUN mkdir -p server/src && \
    echo "fn main() {}" > server/src/main.rs && \
    echo "" > server/src/lib.rs && \
    cargo build --release --package slugsocial-server && \
    rm -rf server/src

# Copy actual source
COPY server/src ./server/src

# Build the actual binary
RUN touch server/src/main.rs && \
    cargo build --release --package slugsocial-server

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

