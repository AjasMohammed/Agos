# === Builder Stage ===
FROM rust:1.82-bookworm AS builder

WORKDIR /usr/src/agentos

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Build release binary
RUN cargo build --release --bin agentctl

# === Runtime Stage ===
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libsqlite3-0 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd --create-home --shell /bin/bash agentos

# Create data directories
RUN mkdir -p /var/lib/agentos/data \
             /var/lib/agentos/tools/core \
             /var/lib/agentos/tools/user \
             /var/log/agentos \
    && chown -R agentos:agentos /var/lib/agentos /var/log/agentos

# Copy binary from builder
COPY --from=builder /usr/src/agentos/target/release/agentctl /usr/local/bin/agentctl

# Copy default config
COPY config/default.toml /etc/agentos/default.toml
# Copy Docker-specific config override
COPY config/docker.toml /etc/agentos/config.toml

# Copy core tool manifests (baked into image, not overwritten by volumes)
COPY tools/core/ /var/lib/agentos/tools/core/
RUN chown -R agentos:agentos /var/lib/agentos/tools/core

USER agentos
WORKDIR /var/lib/agentos

EXPOSE 9091

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:9091/healthz || exit 1

ENTRYPOINT ["agentctl"]
CMD ["start", "--config", "/etc/agentos/config.toml"]
