# syntax=docker/dockerfile:1.7
# === Builder Stage ===
FROM rust:1.91-slim-bookworm AS builder

# pkg-config + libssl-dev needed by transitive deps (fastembed/hf-hub) that
# pull in openssl-sys. OPENSSL_STATIC=1 bakes libssl/libcrypto into the binary
# so the debian bookworm-slim runtime image needs no shared libssl.
# clang + mold — required by .cargo/config.toml which selects clang as the
# linker driver with `-fuse-ld=mold`. mold cuts link time for the monolithic
# agentos binary from tens of seconds down to ~1-2s.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    g++ \
    clang \
    mold \
    && rm -rf /var/lib/apt/lists/*

ENV OPENSSL_STATIC=1 \
    CARGO_INCREMENTAL=0 \
    CARGO_NET_GIT_FETCH_WITH_CLI=true \
    CARGO_TERM_COLOR=always

WORKDIR /usr/src/agentos

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY tools/ tools/
# config/ and skills/core/ are embedded into the binary via rust-embed (see crates/agentos-cli/src/embedded.rs)
COPY config/ config/
COPY skills/ skills/

# Build release binary using BuildKit cache mounts so cargo's registry, git
# index, and the workspace `target/` directory persist across image builds.
# The binary must be `cp`'d out of the cached target/ in the same RUN step
# because cache mounts are not part of the image filesystem.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/usr/src/agentos/target,sharing=locked \
    cargo build --release --bin agentos --features otel \
    && cp target/release/agentos /usr/local/bin/agentos

# === Runtime Stage ===
FROM debian:12-slim

# bubblewrap (bwrap) — required by shell-exec for sandboxed command execution.
# ca-certificates — needed for TLS verification in web-fetch / web-search.
RUN apt-get update && apt-get install -y --no-install-recommends \
    bubblewrap \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create runtime directories and a dedicated non-root user.
RUN groupadd --gid 65532 nonroot \
    && useradd --uid 65532 --gid 65532 --no-create-home --shell /sbin/nologin nonroot \
    && mkdir -p \
        /usr/local/bin \
        /etc/agentos \
        /var/lib/agentos/data \
        /var/lib/agentos/data/models \
        /var/lib/agentos/tools/core \
        /var/lib/agentos/tools/user \
        /var/lib/agentos/plugins/core \
        /var/lib/agentos/plugins/user \
        /var/lib/agentos/static \
        /var/log/agentos \
    && chown -R nonroot:nonroot \
        /var/lib/agentos \
        /var/log/agentos

# Copy binary from builder (staged out of the cached target/ dir in the build step)
COPY --from=builder /usr/local/bin/agentos /usr/local/bin/agentos

# Copy default config
COPY config/default.toml /etc/agentos/default.toml
# Copy Docker-specific config override
COPY config/docker.toml /etc/agentos/config.toml

# Copy core tool manifests (baked into image, not overwritten by volumes)
COPY --chown=nonroot:nonroot tools/core/ /var/lib/agentos/tools/core/

# Copy core plugin manifests; kernel discovers plugins at data_dir.parent()/plugins/{core,user}
COPY --chown=nonroot:nonroot plugins/core/ /var/lib/agentos/plugins/core/

# Copy web UI static assets
COPY --from=builder /usr/src/agentos/crates/agentos-web/static/ /var/lib/agentos/static/

# Set default config path so every agentos command finds it automatically
ENV AGENTOS_CONFIG=/etc/agentos/config.toml
# Point the web server at the static assets directory inside the container
ENV AGENTOS_STATIC_DIR=/var/lib/agentos/static

USER nonroot
WORKDIR /var/lib/agentos

EXPOSE 8080 9091

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/agentos", "healthz"]

ENTRYPOINT ["agentos"]
CMD ["web", "serve", "--host", "0.0.0.0", "--port", "8080"]
