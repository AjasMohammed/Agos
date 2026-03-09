# Plan 06 — Docker Deployment

## Goal

Package AgentOS into a production-ready Docker container with:

- Multi-stage build (small final image, ~47MB target)
- Non-root user
- Health check endpoint
- GPU passthrough support
- `docker-compose.yml` for local development with Ollama
- A `Makefile` for common build/run tasks

---

## Final Image Target

| Component                     | Size       |
| ----------------------------- | ---------- |
| Base `alpine:3.22`            | ~7 MB      |
| Rust kernel binary (stripped) | ~15 MB     |
| Wasmtime shared library       | ~20 MB     |
| Core tool manifests           | <1 MB      |
| SQLCipher (vault)             | ~3 MB      |
| Bubblewrap (bwrap)            | ~200 KB    |
| **Total**                     | **~47 MB** |

---

## Dockerfile

```dockerfile
# ── Stage 1: Build ───────────────────────────────────────────────────
FROM rust:1.85-alpine AS builder

# Build dependencies
RUN apk add --no-cache musl-dev openssl-dev sqlite-dev pkgconfig

WORKDIR /build

# Cache dependencies by copying manifests first
COPY Cargo.toml Cargo.lock ./
# Preserve workspace directory structure for cargo fetch
COPY crates/ crates/
RUN find crates -name "*.rs" -exec sh -c 'mkdir -p "$(dirname "$1")/src" && echo "fn main() {}" > "$(dirname "$1")/src/main.rs"' _ {} \;
RUN cargo fetch

# Full source build
COPY . .
RUN cargo build --release --bin agentos-cli \
    && strip target/release/agentos-cli

# ── Stage 2: Runtime ─────────────────────────────────────────────────
FROM alpine:3.22 AS runtime

# Runtime system dependencies
RUN apk add --no-cache \
    libgcc \
    sqlite-libs \
    openssl-dev \
    bubblewrap \
    ca-certificates \
    && addgroup -S agentos \
    && adduser  -S agentos -G agentos

# Copy binary + assets
COPY --from=builder /build/target/release/agentos-cli /usr/local/bin/agentos-cli
COPY --from=builder /build/tools/core/ /opt/agentos/tools/core/
COPY --from=builder /build/config/default.toml /opt/agentos/config/default.toml

# Runtime directories — owned by non-root user
RUN mkdir -p /opt/agentos/data \
             /opt/agentos/tools/user \
             /opt/agentos/vault \
             /opt/agentos/logs \
    && chown -R agentos:agentos /opt/agentos

WORKDIR /opt/agentos
USER agentos

# Persistent volumes
VOLUME ["/opt/agentos/data", "/opt/agentos/tools/user", "/opt/agentos/vault"]

# Ports
EXPOSE 8080    # Web UI
EXPOSE 9090    # Kernel IPC (optional external access)

# Health check — the kernel exposes GET /health
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:8080/health || exit 1

ENTRYPOINT ["/usr/local/bin/agentos-cli", "start", "--config", "/opt/agentos/config/default.toml"]
```

---

## `/health` Endpoint

Add to the Axum web server (Plan 05):

```rust
// handlers/health.rs
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.kernel.health_status().await;
    let code = if status.healthy { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(status))
}

pub struct HealthStatus {
    pub healthy: bool,
    pub uptime_seconds: u64,
    pub connected_agents: usize,
    pub task_queue_depth: usize,
    pub vault_locked: bool,
}
```

Also add a `/ready` endpoint that returns 200 only after the vault is unlocked and tool registry is loaded.

---

## docker-compose.yml (Local Development)

```yaml
# docker-compose.yml
services:
    agentos:
        build:
            context: .
            target: runtime
        image: agentos:latest
        container_name: agentos-kernel
        restart: unless-stopped
        ports:
            - "8080:8080" # Web UI
        volumes:
            - ./data:/opt/agentos/data # Persistent data + task history
            - ./tools/user:/opt/agentos/tools/user # User-installed tools
            - agentos-vault:/opt/agentos/vault # Encrypted secrets vault
        environment:
            - AGENTOS_ENV=development
            - AGENTOS_LOG_LEVEL=info
        depends_on:
            ollama:
                condition: service_healthy
        healthcheck:
            test: ["CMD", "wget", "-qO-", "http://localhost:8080/health"]
            interval: 30s
            timeout: 5s
            retries: 3
            start_period: 10s

        # NOTE: No API keys in environment variables.
        # Run: docker exec -it agentos-kernel agentos-cli secret set OPENAI_API_KEY

    ollama:
        image: ollama/ollama:latest
        container_name: ollama
        restart: unless-stopped
        volumes:
            - ollama-models:/root/.ollama
        healthcheck:
            test: ["CMD", "ollama", "list"]
            interval: 30s
            timeout: 5s
            retries: 5
            start_period: 30s

    # Optional: pull a model on startup
    ollama-pull:
        image: ollama/ollama:latest
        restart: "no"
        depends_on:
            ollama:
                condition: service_healthy
        entrypoint: ["ollama", "pull", "llama3.2"]
        environment:
            - OLLAMA_HOST=ollama:11434

volumes:
    agentos-vault: # Named volume — encrypted vault, survives container restarts
    ollama-models: # Named volume — downloaded LLM weights
```

## docker-compose-gpu.yml (NVIDIA GPU override)

```yaml
# docker-compose-gpu.yml
services:
    agentos:
        deploy:
            resources:
                reservations:
                    devices:
                        - driver: nvidia
                          count: all
                          capabilities: [gpu]
        environment:
            - NVIDIA_VISIBLE_DEVICES=all
            - NVIDIA_DRIVER_CAPABILITIES=compute,utility

    ollama:
        deploy:
            resources:
                reservations:
                    devices:
                        - driver: nvidia
                          count: all
                          capabilities: [gpu]
        environment:
            - NVIDIA_VISIBLE_DEVICES=all
```

Usage:

```bash
docker compose -f docker-compose.yml -f docker-compose-gpu.yml up
```

---

## `.dockerignore`

```
.git
.gitignore
target/
node_modules/
**/*.log
**/*.swp
docs/
v1-plans/
v2-plans/
v3-plans/
agentic-os-updated.md
.env
.env.*
*.pem
*.key
```

---

## Makefile

```makefile
.PHONY: build run stop logs health shell test

IMAGE   := agentos:latest
COMPOSE := docker compose

build:
	$(COMPOSE) build

run:
	$(COMPOSE) up -d

run-gpu:
	$(COMPOSE) -f docker-compose.yml -f docker-compose-gpu.yml up -d

stop:
	$(COMPOSE) down

logs:
	$(COMPOSE) logs -f agentos

health:
	curl -s http://localhost:8080/health | jq .

shell:
	docker exec -it agentos-kernel /bin/sh

# Run CLI commands inside the container
cli:
	docker exec -it agentos-kernel agentos-cli $(ARGS)

# Example: make cli ARGS="tool list"
# Example: make cli ARGS="agent connect --provider ollama --model llama3.2 --name dev"

test:
	cargo test --workspace

release:
	cargo build --release --bin agentos-cli
	docker build -t $(IMAGE) .
```

---

## Configuration for Docker

### `config/default.toml` (updated for container paths)

```toml
[kernel]
data_dir      = "/opt/agentos/data"
log_dir       = "/opt/agentos/logs"
socket_path   = "/opt/agentos/agentos.sock"

[tools]
core_tools_dir = "/opt/agentos/tools/core"
user_tools_dir = "/opt/agentos/tools/user"
data_dir       = "/opt/agentos/data"

[vault]
path = "/opt/agentos/vault/vault.db"

[web]
enabled = true
bind    = "0.0.0.0:8080"

[logging]
level  = "info"
format = "json"    # Structured JSON for container log aggregation
```

---

## Graceful Shutdown

The kernel must handle `SIGTERM` (Docker sends this on `docker stop`):

```rust
// In kernel.rs boot() — after starting:
let kernel_for_shutdown = Arc::clone(&kernel_arc);
tokio::spawn(async move {
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate()).unwrap();
    let mut sigint  = tokio::signal::unix::signal(SignalKind::interrupt()).unwrap();
    tokio::select! {
        _ = sigterm.recv() => { tracing::info!("SIGTERM received — beginning graceful shutdown"); }
        _ = sigint.recv()  => { tracing::info!("SIGINT received — beginning graceful shutdown"); }
    }
    kernel_for_shutdown.shutdown().await;
});
```

`Kernel::shutdown()`:

1. Stop accepting new tasks
2. Wait for running tasks to complete (up to 30s)
3. Flush audit log
4. Lock the vault
5. Close all LLM connections

---

## Tests

```bash
# Build the image
docker build -t agentos:test .

# Confirm image size is acceptable
docker image inspect agentos:test --format '{{.Size}}' | \
  awk '{printf "%.1f MB\n", $1/1024/1024}'

# Start container + health check passes
docker run -d --name agentos-test -p 8080:8080 agentos:test
sleep 5
curl -f http://localhost:8080/health

# Connect an agent inside the container
docker exec agentos-test agentos-cli agent connect \
  --provider ollama --model llama3.2 --name test-agent

docker exec agentos-test agentos-cli tool list

# Confirm vault survives container restart
docker restart agentos-test
sleep 5
curl -f http://localhost:8080/health

# Teardown
docker rm -f agentos-test
```

---

## CI/CD Integration

```yaml
# .github/workflows/docker.yml
name: Docker Build

on:
    push:
        branches: [main]
        tags: ["v*"]

jobs:
    build:
        runs-on: ubuntu-latest
        steps:
            - uses: actions/checkout@v4

            - name: Build image
              run: docker build -t agentos:${{ github.sha }} .

            - name: Test health check
              run: |
                  docker run -d --name test -p 8080:8080 agentos:${{ github.sha }}
                  sleep 10
                  curl -f http://localhost:8080/health

            - name: Run tests inside container
              run: docker exec test agentos-cli status

            - name: Login to GHCR
              if: startsWith(github.ref, 'refs/tags/')
              uses: docker/login-action@v3
              with:
                  registry: ghcr.io
                  username: ${{ github.actor }}
                  password: ${{ secrets.GITHUB_TOKEN }}

            - name: Tag + push (on tag)
              if: startsWith(github.ref, 'refs/tags/')
              run: |
                  docker tag agentos:${{ github.sha }} ghcr.io/${{ github.repository }}:${{ github.ref_name }}
                  docker push ghcr.io/${{ github.repository }}:${{ github.ref_name }}
```

> [!IMPORTANT]
> The vault passphrase must never appear in CI environment variables, Dockerfile `ENV`, or GitHub Secrets used in `docker run`. In automated deployments, use a KDF with a machine-derived key or a hardware TPM — covered in Phase 4 (Agent Identity Persistence).
