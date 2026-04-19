---
title: Docker Deployment Artifacts
tags:
  - cli
  - v3
  - plan
date: 2026-03-13
status: complete
effort: 4h
priority: medium
---

# Docker Deployment Artifacts

> Create a multi-stage Dockerfile and docker-compose.yml for single-node AgentOS deployment, addressing the "Missing canonical Docker deployment artifacts" blocker from Program 16.

---

## Why This Phase

The First Deployment Readiness Program (Program 16) identified "Missing canonical Docker deployment artifacts" as an open blocker. As of 2026-03-13, the repository has no `Dockerfile` or `docker-compose.yml`. Without these, there is no reproducible deployment path for AgentOS beyond building from source.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Dockerfile | Does not exist | Multi-stage build: Rust builder + slim runtime |
| docker-compose.yml | Does not exist | Single-service composition with volume mounts for data persistence |
| Health check | No HTTP health endpoint | `/healthz` endpoint via `agentos-web` or simple file-based check |
| Config | `config/default.toml` uses `/tmp/` paths | Docker config override uses `/var/lib/agentos/` paths |

---

## What to Do

### 1. Create `Dockerfile`

Create `Dockerfile` at the repository root.

```dockerfile
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

USER agentos
WORKDIR /var/lib/agentos

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD test -S /var/lib/agentos/data/agentos.sock || exit 1

ENTRYPOINT ["agentctl"]
CMD ["start", "--config", "/etc/agentos/config.toml"]
```

### 2. Create `config/docker.toml`

Create a Docker-specific configuration that uses persistent paths:

```toml
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 300
context_window_max_entries = 200
context_window_token_budget = 128000
max_iterations_per_task = 20

[bus]
socket_path = "/var/lib/agentos/data/agentos.sock"

[audit]
log_path = "/var/lib/agentos/data/audit.db"

[secrets]
vault_path = "/var/lib/agentos/data/vault.db"

[tools]
core_tools_dir = "/var/lib/agentos/tools/core"
user_tools_dir = "/var/lib/agentos/tools/user"
data_dir = "/var/lib/agentos/data"

[ollama]
host = "http://ollama:11434"

[memory]
model_cache_dir = "/var/lib/agentos/data/models"
```

### 3. Create `docker-compose.yml`

Create `docker-compose.yml` at the repository root:

```yaml
version: "3.8"

services:
  agentos:
    build: .
    container_name: agentos-kernel
    environment:
      - AGENTOS_VAULT_PASSPHRASE=${AGENTOS_VAULT_PASSPHRASE:-changeme}
    volumes:
      - agentos-data:/var/lib/agentos/data
      - agentos-tools:/var/lib/agentos/tools
    ports:
      - "8080:8080"
    restart: unless-stopped
    depends_on:
      ollama:
        condition: service_started

  ollama:
    image: ollama/ollama:latest
    container_name: agentos-ollama
    volumes:
      - ollama-models:/root/.ollama
    ports:
      - "11434:11434"

volumes:
  agentos-data:
  agentos-tools:
  ollama-models:
```

### 4. Create `.dockerignore`

Create `.dockerignore` at the repository root:

```
target/
.git/
.github/
obsidian-vault/
v1-plans/
v2-plans/
v3-plans/
docs/
*.md
.claude/
```

### 5. Update CLI to accept vault passphrase from environment variable

Open `crates/agentos-cli/src/main.rs`, function `cmd_start`.

The current code reads the passphrase from `--vault-passphrase` flag or interactive prompt. Add environment variable fallback:

```rust
let passphrase = match vault_passphrase {
    Some(p) => p,
    None => {
        if let Ok(env_pass) = std::env::var("AGENTOS_VAULT_PASSPHRASE") {
            env_pass
        } else {
            eprint!("Enter vault passphrase: ");
            rpassword::read_password()?
        }
    }
};
```

---

## Files Changed

| File | Change |
|------|--------|
| `Dockerfile` | New file -- multi-stage build |
| `docker-compose.yml` | New file -- single-node composition with Ollama |
| `.dockerignore` | New file -- exclude build artifacts and docs |
| `config/docker.toml` | New file -- Docker-specific config with persistent paths |
| `crates/agentos-cli/src/main.rs` | Read vault passphrase from `AGENTOS_VAULT_PASSPHRASE` env var as fallback |

---

## Prerequisites

[[01-clippy-ci-gate-fixes]] should be complete first so the Docker build succeeds without clippy errors (if CI runs clippy in the build stage).

---

## Test Plan

- `docker build -t agentos .` must complete successfully
- `docker compose up -d` must start both `agentos` and `ollama` services
- `docker compose ps` must show both services as "Up"
- `docker exec agentos-kernel agentctl status` must connect to the kernel
- Verify that data persists across `docker compose down && docker compose up -d` (check audit log exists in volume)
- Verify that `.dockerignore` excludes `target/` and `obsidian-vault/` from the build context

---

## Verification

```bash
# Build
docker build -t agentos:test .

# Verify binary exists in image
docker run --rm agentos:test --version

# Compose up (requires Docker Compose v2)
docker compose up -d
docker compose ps
docker compose down
```
