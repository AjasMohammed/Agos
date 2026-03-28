---
title: Deployment Packaging
tags:
  - deployment
  - docker
  - kubernetes
  - devops
  - plan
  - v3
date: 2026-03-25
status: completed
effort: 3d
priority: medium
---

# Phase 8 — Deployment Packaging

> Package AgentOS as a Docker image and Helm chart so teams can deploy it in Kubernetes or with Docker Compose in under 10 minutes. This removes the biggest enterprise trial blocker: "How do I deploy this?"

---

## Why This Phase

The research is explicit:

> "While no-code platforms allow rapid prototyping, developer SDKs are the standard for core business applications requiring deep integration. However, technically excellent systems fail if deployment is painful."

Currently, deploying AgentOS requires:
1. Rust toolchain installation
2. `cargo build --workspace --release`
3. Manual systemd unit setup
4. Manual config file creation
5. Manual vault initialization

This is unacceptable for enterprise trials. The target is: **`docker run agentos/kernel`** starts a production-ready kernel with sane defaults.

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Distribution | Source only (cargo build) | Docker image on Docker Hub (multi-arch: amd64, arm64) |
| Kubernetes | None | Helm chart with configurable replicas, secrets, volumes |
| Docker Compose | None | `docker-compose.yml` quickstart (kernel + web UI + Jaeger) |
| Configuration | Manual config/default.toml edit | Environment variable overrides + Kubernetes ConfigMap |
| Vault init | Manual `agentctl secret init` | Auto-init on first boot with generated key |
| Health checks | `/healthz` endpoint (exists) | Docker HEALTHCHECK + Kubernetes liveness/readiness probes |
| Multi-tenancy | Single kernel, single user | Namespace isolation via Kubernetes namespaces |
| Upgrade story | Manual rebuild | `helm upgrade agentos agentos/kernel --set image.tag=v1.2.0` |

---

## Detailed Subtasks

### Subtask 8.1 — Dockerfile (multi-stage, minimal)

**File:** `Dockerfile` (new, at repo root)

```dockerfile
# Stage 1: Build (Rust toolchain)
FROM rust:1.86-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/

# Build only the kernel binary (release, statically linked)
RUN RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --bin agentctl -p agentos-cli

# Stage 2: Runtime (minimal Debian)
FROM debian:bookworm-slim AS runtime

# Install runtime deps: sqlite3 (for audit/memory DBs), ca-certificates (for TLS)
RUN apt-get update && apt-get install -y --no-install-recommends \
    sqlite3 ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -u 1001 agentos

# Copy binary + tools
COPY --from=builder /build/target/release/agentctl /usr/local/bin/
COPY tools/ /opt/agentos/tools/
COPY config/default.toml /etc/agentos/config.toml

# Data directory (vault, memory DBs, audit log)
RUN mkdir -p /var/lib/agentos && chown agentos:agentos /var/lib/agentos

USER agentos
WORKDIR /var/lib/agentos

# Expose web UI + kernel socket
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD agentctl healthz || exit 1

# Default: run kernel (web mode)
ENTRYPOINT ["agentctl"]
CMD ["web", "serve", "--bind", "0.0.0.0:8080"]
```

**Note on seccomp:** The seccomp-BPF sandbox requires `CAP_SYS_ADMIN` or `--privileged` in Docker. Document this clearly in the README. For environments without this capability, seccomp is disabled automatically (the kernel detects this and logs a warning).

---

### Subtask 8.2 — Docker Compose quickstart

**File:** `docker-compose.yml` (new, at repo root)

```yaml
version: "3.9"

services:
  agentos:
    image: agentos/kernel:latest
    ports:
      - "8080:8080"
    volumes:
      - agentos-data:/var/lib/agentos
      - ./config/local.toml:/etc/agentos/config.toml:ro
    environment:
      - AGENTOS_LLM_PROVIDER=anthropic
      - AGENTOS_LLM_API_KEY=${ANTHROPIC_API_KEY}
      - AGENTOS_OTEL_ENDPOINT=http://jaeger:4317
    depends_on:
      - jaeger
    restart: unless-stopped

  jaeger:
    image: jaegertracing/all-in-one:latest
    ports:
      - "16686:16686"    # Jaeger UI
      - "4317:4317"      # OTLP gRPC
    restart: unless-stopped

volumes:
  agentos-data:
```

Also provide a minimal **`.env.example`**:

```bash
# Required: choose one LLM provider
ANTHROPIC_API_KEY=sk-ant-...
# OPENAI_API_KEY=sk-...
# OLLAMA_HOST=http://localhost:11434
```

---

### Subtask 8.3 — Environment variable config overrides

**File:** `crates/agentos-kernel/src/config.rs`

Add environment variable override layer so Docker/Kubernetes users don't need to modify TOML files:

```rust
pub fn load_config() -> Result<KernelConfig> {
    let mut config = config::Config::builder()
        .add_source(config::File::with_name("/etc/agentos/config"))
        .add_source(config::File::with_name("config/default").required(false))
        // Environment overrides: AGENTOS_LLM_PROVIDER → config.llm.provider
        .add_source(config::Environment::with_prefix("AGENTOS").separator("_"))
        .build()?;
    Ok(config.try_deserialize::<KernelConfig>()?)
}
```

Key environment variables documented:

| Env Var | Config Key | Example |
|---------|-----------|---------|
| `AGENTOS_LLM_PROVIDER` | `llm.provider` | `anthropic` |
| `AGENTOS_LLM_API_KEY` | `llm.api_key` | `sk-ant-...` |
| `AGENTOS_LLM_MODEL` | `llm.model` | `claude-sonnet-4-6` |
| `AGENTOS_DATA_DIR` | `kernel.data_dir` | `/var/lib/agentos` |
| `AGENTOS_WEB_PORT` | `web.port` | `8080` |
| `AGENTOS_OTEL_ENABLED` | `otel.enabled` | `true` |
| `AGENTOS_OTEL_ENDPOINT` | `otel.endpoint` | `http://jaeger:4317` |
| `AGENTOS_VAULT_KEY` | `vault.master_key` | (base64 key) |

---

### Subtask 8.4 — Vault auto-initialization

**File:** `crates/agentos-kernel/src/kernel.rs`

On first boot, if no vault exists at `data_dir/vault.db`, auto-initialize with a generated key:

```rust
async fn init_vault(config: &VaultConfig, data_dir: &Path) -> Result<VaultStore> {
    let vault_path = data_dir.join("vault.db");
    if !vault_path.exists() {
        tracing::info!("First boot: initializing vault");

        // Generate or load master key from env/config
        let master_key = match std::env::var("AGENTOS_VAULT_KEY") {
            Ok(key) => base64::decode(&key)?,
            Err(_) => {
                // Generate random key, write to data_dir/vault.key (warn user to back it up)
                let key = generate_random_key();
                let key_path = data_dir.join("vault.key");
                std::fs::write(&key_path, base64::encode(&key))?;
                tracing::warn!(
                    "Generated vault key saved to {:?}. Back this up — losing it means losing all secrets.",
                    key_path
                );
                key
            }
        };
        VaultStore::initialize(&vault_path, &master_key)?;
    }
    VaultStore::open(&vault_path, &get_master_key(config)?)
}
```

---

### Subtask 8.5 — Helm chart

**Directory:** `deploy/helm/agentos/` (new)

```
deploy/helm/agentos/
├── Chart.yaml
├── values.yaml
├── templates/
│   ├── deployment.yaml
│   ├── service.yaml
│   ├── configmap.yaml
│   ├── secret.yaml
│   ├── pvc.yaml
│   ├── serviceaccount.yaml
│   └── ingress.yaml
```

**Chart.yaml:**
```yaml
apiVersion: v2
name: agentos
description: AgentOS — LLM-native agent operating system
type: application
version: 0.1.0
appVersion: "0.1.0"
```

**values.yaml (key sections):**
```yaml
image:
  repository: agentos/kernel
  tag: "latest"
  pullPolicy: IfNotPresent

replicas: 1

persistence:
  enabled: true
  size: 10Gi
  storageClass: ""

llm:
  provider: anthropic
  model: claude-sonnet-4-6
  # api_key: set via secret

web:
  port: 8080
  ingress:
    enabled: false
    host: ""

otel:
  enabled: false
  endpoint: ""

resources:
  requests:
    cpu: 250m
    memory: 512Mi
  limits:
    cpu: 2000m
    memory: 2Gi

securityContext:
  runAsNonRoot: true
  runAsUser: 1001
  readOnlyRootFilesystem: true
```

**templates/deployment.yaml** includes:
- Liveness probe: `GET /healthz` every 30s
- Readiness probe: `GET /healthz` every 10s with 5s initial delay
- Secret mount for `AGENTOS_LLM_API_KEY` and `AGENTOS_VAULT_KEY`
- PVC mount for `/var/lib/agentos`
- ConfigMap for `config.toml`

---

### Subtask 8.6 — GitHub Actions: build and push Docker image

**File:** `.github/workflows/docker.yml` (new)

```yaml
on:
  push:
    tags: ["v*"]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          username: ${{ secrets.DOCKERHUB_USERNAME }}
          password: ${{ secrets.DOCKERHUB_TOKEN }}
      - uses: docker/build-push-action@v5
        with:
          platforms: linux/amd64,linux/arm64
          push: true
          tags: |
            agentos/kernel:latest
            agentos/kernel:${{ github.ref_name }}
          cache-from: type=gha
          cache-to: type=gha,mode=max
```

---

## Files Changed

| File | Change |
|------|--------|
| `Dockerfile` | New — multi-stage build |
| `docker-compose.yml` | New — quickstart with Jaeger |
| `.env.example` | New — environment variable documentation |
| `deploy/helm/agentos/` | New — Helm chart directory |
| `crates/agentos-kernel/src/config.rs` | Modified — env var override layer |
| `crates/agentos-kernel/src/kernel.rs` | Modified — vault auto-init on first boot |
| `.github/workflows/docker.yml` | New — CI build + push pipeline |

---

## Dependencies

- No other phases required
- All kernel features (existing) must be complete and passing tests

---

## Test Plan

1. **Docker build succeeds** — `docker build -t agentos-test .` completes without error
2. **Container starts** — `docker run agentos-test web serve` starts without panics, `/healthz` returns 200
3. **First boot vault init** — fresh container with no data volume auto-creates vault.key, logs warning
4. **Env var override** — set `AGENTOS_LLM_PROVIDER=ollama`, verify config loaded value
5. **Docker Compose** — `docker-compose up`, connect to http://localhost:8080, verify web UI loads
6. **Helm lint** — `helm lint deploy/helm/agentos/` — no errors or warnings
7. **Helm template** — `helm template agentos deploy/helm/agentos/` — valid Kubernetes YAML
8. **Kubernetes deploy** — deploy to local kind cluster, verify pod reaches Running state and healthcheck passes

---

## Verification

```bash
# Docker
docker build -t agentos/kernel:dev .
docker run --rm -e AGENTOS_LLM_PROVIDER=anthropic \
           -e AGENTOS_LLM_API_KEY=test \
           -p 8080:8080 agentos/kernel:dev

# Docker Compose
cp .env.example .env && echo "ANTHROPIC_API_KEY=..." >> .env
docker-compose up -d
curl http://localhost:8080/healthz

# Helm
helm lint deploy/helm/agentos/
helm template agentos deploy/helm/agentos/ | kubectl apply --dry-run=client -f -
```

## Implementation Status

Completed in repo with:
- Opt-in vault auto-bootstrap for deployment environments via `AGENTOS_AUTO_INIT_VAULT`
- Expanded runtime environment overrides for deployment-friendly config control
- Updated Docker Compose packaging with Jaeger, Ollama, persistent volumes, and explicit config wiring
- Helm chart scaffolding under `deploy/helm/agentos/`
- Docker image publish workflow under `.github/workflows/docker.yml`

Verification completed:
- `cargo fmt --all`
- `cargo test -p agentos-kernel -p agentos-cli`
- Reviewer pass completed after fixes

Remaining verification gap:
- `helm lint` / `helm template` could not be executed in this environment because `helm` is not installed locally

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[06-opentelemetry-export]] — Jaeger included in Docker Compose quickstart
