FROM rust:1.86-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev curl git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo fetch

COPY src/ src/
RUN cargo build --release && strip target/release/knot-server

FROM debian:bookworm-slim

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
