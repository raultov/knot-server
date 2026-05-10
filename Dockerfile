FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev curl git ca-certificates g++ \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo fetch

COPY src/ src/
RUN cargo build --release && strip target/release/knot-server

FROM debian:trixie-slim

LABEL org.opencontainers.image.title="knot-server"
LABEL org.opencontainers.image.description="Distributed REST API server for knot codebase indexing. Manages Git repositories across a cluster with shared workspace coordination."
LABEL org.opencontainers.image.url="https://github.com/raultov/knot-server"
LABEL org.opencontainers.image.source="https://github.com/raultov/knot-server"
LABEL org.opencontainers.image.licenses="MIT"
LABEL org.opencontainers.image.vendor="raultov"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates git openssh-client \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/knot-server /usr/local/bin/knot-server

RUN mkdir -p /var/lib/knot/repos
VOLUME /var/lib/knot/repos

EXPOSE 3000

ENV KNOT_SERVER_PORT=3000
ENV KNOT_SERVER_BIND_ADDR=0.0.0.0
ENV KNOT_WORKSPACE_DIR=/var/lib/knot/repos

ENTRYPOINT ["knot-server"]
