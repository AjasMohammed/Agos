# === Builder Stage ===
FROM rust:1.88-slim-bookworm AS builder

# pkg-config + libssl-dev needed by transitive deps (fastembed/hf-hub) that
# pull in openssl-sys. OPENSSL_STATIC=1 bakes libssl/libcrypto into the binary
# so the distroless runtime image needs no shared libssl.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    g++ \
    && rm -rf /var/lib/apt/lists/*

ENV OPENSSL_STATIC=1

WORKDIR /usr/src/agentos

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY tools/ tools/

# Build release binary
RUN cargo build --release --bin agentctl

# Create empty dirs for the runtime stage (distroless has no mkdir)
RUN mkdir -p runtime-dirs/data runtime-dirs/models runtime-dirs/user-tools runtime-dirs/log

# === Runtime Stage ===
FROM gcr.io/distroless/cc-debian12

# distroless/cc-debian12 provides glibc + libgcc — no apt needed.
# SQLite is statically bundled (rusqlite feature = "bundled").
# All runtime libs (OpenSSL eliminated via rustls-tls) are self-contained.

# Copy binary from builder
COPY --from=builder /usr/src/agentos/target/release/agentctl /usr/local/bin/agentctl

# Create writable data directories (distroless has no mkdir/shell).
# Docker initialises new volumes with the image directory's ownership,
# so nonroot can write to these paths when volumes are mounted.
COPY --from=builder --chown=nonroot:nonroot /usr/src/agentos/runtime-dirs/data/ /var/lib/agentos/data/
COPY --from=builder --chown=nonroot:nonroot /usr/src/agentos/runtime-dirs/models/ /var/lib/agentos/data/models/
COPY --from=builder --chown=nonroot:nonroot /usr/src/agentos/runtime-dirs/user-tools/ /var/lib/agentos/tools/user/
COPY --from=builder --chown=nonroot:nonroot /usr/src/agentos/runtime-dirs/log/ /var/log/agentos/

# Copy default config
COPY config/default.toml /etc/agentos/default.toml
# Copy Docker-specific config override
COPY config/docker.toml /etc/agentos/config.toml

# Copy core tool manifests (baked into image, not overwritten by volumes)
COPY --chown=nonroot:nonroot tools/core/ /var/lib/agentos/tools/core/

# Copy web UI static assets
COPY --from=builder /usr/src/agentos/crates/agentos-web/static/ /var/lib/agentos/static/

# Set default config path so every agentctl command finds it automatically
ENV AGENTOS_CONFIG=/etc/agentos/config.toml
# Point the web server at the static assets directory inside the container
ENV AGENTOS_STATIC_DIR=/var/lib/agentos/static

# Use distroless built-in unprivileged user (uid 65532)
USER nonroot
WORKDIR /var/lib/agentos

EXPOSE 8080 9091

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/agentctl", "healthz"]

ENTRYPOINT ["agentctl"]
CMD ["web", "serve", "--host", "0.0.0.0", "--port", "8080"]
