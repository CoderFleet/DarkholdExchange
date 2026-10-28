# The Rust gateway (engine, ledger, REST and WebSocket API) as a small image.
# Build from the repository root:  docker build -f deploy/gateway.Dockerfile .

# ---- stage 1: compile ------------------------------------------------------------------------
# Same Rust version as rust-toolchain.toml. If this tag does not exist, change both together.
FROM rust:1.99.1-bookworm AS build
# The image already has this exact toolchain; this stops rustup looking for the components that
# rust-toolchain.toml lists (rustfmt, clippy), which a release build does not need.
ENV RUSTUP_TOOLCHAIN=1.99.1
WORKDIR /app
COPY . .
# The cache mounts keep downloaded crates and compiled dependencies between builds, so a small code
# change does not recompile everything.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked -p dx-gateway \
    && cp target/release/dx-gateway /dx-gateway

# ---- stage 2: run ----------------------------------------------------------------------------
FROM debian:bookworm-slim
# curl is only for the health check. ca-certificates lets the program trust HTTPS sites.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/darkhold --shell /usr/sbin/nologin darkhold \
    && mkdir -p /var/lib/darkhold \
    && chown darkhold /var/lib/darkhold
COPY --from=build /dx-gateway /usr/local/bin/dx-gateway
# The write-ahead log is written under here (data/log). Mount a volume on it to keep it.
ENV DX_WAL_DIR=/var/lib/darkhold \
    GATEWAY_ADDR=0.0.0.0:8080
WORKDIR /var/lib/darkhold
USER darkhold
EXPOSE 8080
HEALTHCHECK --interval=5s --timeout=3s --retries=12 CMD curl -fsS http://localhost:8080/api/health || exit 1
CMD ["dx-gateway"]