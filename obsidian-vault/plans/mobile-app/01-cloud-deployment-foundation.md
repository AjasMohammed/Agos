---
title: Phase 1 — Cloud Deployment Foundation
tags:
  - mobile
  - deployment
  - docker
  - phase-1
date: 2026-04-19
status: planned
effort: 3d
priority: high
---

# Phase 1 — Cloud Deployment Foundation

> Make AgentOS runnable as a cloud service: Dockerize the kernel + API, externalize config via env vars, expose `agentos-api` over TLS behind a reverse proxy, and document a reference deploy. No mobile-specific code yet — this is the substrate every other phase builds on.

---

## Why this phase

Today AgentOS assumes a workstation: kernel writes to `~/.agentos/`, CLI connects via Unix socket, vault key is prompted interactively. None of that works in a cloud environment where the mobile app needs to reach the API over HTTPS. Before any mobile code is written, we need a **single command** that stands up a hardened AgentOS instance accessible at a public HTTPS URL.

## Current → Target state

**Current:**
- No `Dockerfile`.
- Binary reads config from `config/default.toml` at CWD.
- Vault password read from TTY via `rpassword`.
- API binds to `127.0.0.1:8080`.
- TLS terminated nowhere — no HTTPS.

**Target:**
- `Dockerfile` at repo root producing a distroless image running `agentos serve`.
- `deploy/docker-compose.yml` for a reference single-node deploy.
- `deploy/caddy/Caddyfile` reverse-proxy with automatic Let's Encrypt.
- Config loader supports env-var overrides with `AGENTOS__` prefix (double-underscore separator).
- Vault password read from `AGENTOS_VAULT_PASSWORD` env if TTY absent.
- API binds `0.0.0.0:8080` inside container; Caddy exposes `443` externally.
- Health endpoint `/healthz` returns 200 without auth; `/readyz` returns 200 only when kernel ready.

## Detailed subtasks

### 1.1 Add env-var config overrides

File: `crates/agentos-kernel/src/config.rs` (or wherever `Config::load` lives — verify with `grep -n "fn load" crates/agentos-kernel/src/config.rs`).

Use `config` crate's `Environment` source:

```rust
use config::{Config as CfgBuilder, Environment, File};

pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
    let mut builder = CfgBuilder::builder();
    if let Some(p) = path {
        builder = builder.add_source(File::from(p));
    } else {
        builder = builder.add_source(File::with_name("config/default"));
    }
    builder = builder.add_source(
        Environment::with_prefix("AGENTOS")
            .separator("__")
            .try_parsing(true)
    );
    builder.build()?.try_deserialize()
}
```

`AGENTOS__KERNEL__DATA_DIR=/data` overrides `[kernel].data_dir`. Document the mapping in `deploy/README.md`.

### 1.2 Add non-interactive vault unlock

File: `crates/agentos-vault/src/lib.rs` (verify path with `grep -rn "rpassword" crates/agentos-vault/`).

Add a helper:

```rust
pub fn vault_password_from_env_or_tty() -> Result<ZeroizingString, VaultError> {
    if let Ok(p) = std::env::var("AGENTOS_VAULT_PASSWORD") {
        return Ok(ZeroizingString::from(p));
    }
    if atty::is(atty::Stream::Stdin) {
        let p = rpassword::prompt_password("Vault password: ")?;
        return Ok(ZeroizingString::from(p));
    }
    Err(VaultError::NoPasswordProvided)
}
```

Replace the existing `rpassword::prompt_password` call sites with this helper. Add a test that sets the env var in a `serial_test` block.

**Security note:** never log the password. Add an `AuditEventType::VaultUnlockedFromEnv` audit entry so operators can audit when env-var unlock is used.

### 1.3 Add `/healthz` and `/readyz` endpoints

File: `crates/agentos-api/src/handlers/system.rs` (verify with `grep -n "healthz\|readyz" crates/agentos-api/src/handlers/system.rs`).

```rust
pub async fn healthz() -> StatusCode { StatusCode::OK }

pub async fn readyz(State(app): State<AppState>) -> StatusCode {
    if app.kernel.is_ready().await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
```

Wire into the router (`crates/agentos-api/src/service.rs`) **before** the auth middleware layer — health must be callable by load balancers without a token.

### 1.4 Bind 0.0.0.0 in server mode

In `crates/agentos-api/src/service.rs`, make the bind address configurable (`config.api.bind_addr`, default `127.0.0.1:8080`). In the container we set `AGENTOS__API__BIND_ADDR=0.0.0.0:8080`.

### 1.5 Create `Dockerfile`

File: `Dockerfile` (new, repo root).

```dockerfile
# syntax=docker/dockerfile:1.7
FROM rust:1.83-slim-bookworm AS builder
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
COPY . .
RUN --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --workspace --bin agentos && \
    cp target/release/agentos /agentos

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /agentos /usr/local/bin/agentos
COPY config /etc/agentos/config
COPY skills/core /etc/agentos/skills/core
COPY plugins/core /etc/agentos/plugins/core
COPY tools/core /etc/agentos/tools/core
VOLUME ["/data"]
ENV AGENTOS__KERNEL__DATA_DIR=/data \
    AGENTOS__API__BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/agentos", "serve"]
```

### 1.6 Add `agentos serve` CLI subcommand

File: `crates/agentos-cli/src/commands/serve.rs` (new). Dispatch in `crates/agentos-cli/src/main.rs`.

Behavior: boot kernel + API in-process (no Unix socket), block on SIGINT/SIGTERM for graceful shutdown (propagate `CancellationToken`).

```rust
pub async fn run(config_path: Option<PathBuf>) -> Result<()> {
    let config = Config::load(config_path.as_deref())?;
    let password = vault_password_from_env_or_tty()?;
    let kernel = Kernel::boot(config.clone(), password).await?;
    let api = agentos_api::Service::new(kernel.clone(), config.api.clone());
    let shutdown = CancellationToken::new();
    tokio::spawn({
        let token = shutdown.clone();
        async move {
            let mut term = signal(SignalKind::terminate()).unwrap();
            let mut int = signal(SignalKind::interrupt()).unwrap();
            tokio::select! {
                _ = term.recv() => {},
                _ = int.recv() => {},
            }
            token.cancel();
        }
    });
    api.run(shutdown).await
}
```

### 1.7 Reference docker-compose + Caddy

Files:
- `deploy/docker-compose.yml`
- `deploy/caddy/Caddyfile`
- `deploy/README.md`

`docker-compose.yml`:

```yaml
services:
  agentos:
    build: ..
    restart: unless-stopped
    environment:
      AGENTOS_VAULT_PASSWORD: ${AGENTOS_VAULT_PASSWORD:?set in .env}
      AGENTOS__LLM__PROVIDER: ${AGENTOS__LLM__PROVIDER:-anthropic}
      AGENTOS__LLM__API_KEY_ENV: ${AGENTOS__LLM__API_KEY_ENV}
    volumes:
      - agentos-data:/data
    expose:
      - "8080"
    healthcheck:
      test: ["CMD", "/usr/local/bin/agentos", "version"]
      interval: 30s
  caddy:
    image: caddy:2
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./caddy/Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy-data:/data
      - caddy-config:/config
    depends_on:
      - agentos
volumes:
  agentos-data:
  caddy-data:
  caddy-config:
```

`Caddyfile`:

```
{$AGENTOS_DOMAIN} {
    encode gzip zstd
    reverse_proxy agentos:8080
}
```

## Files changed

| File | Change |
|------|--------|
| `Dockerfile` | new |
| `deploy/docker-compose.yml` | new |
| `deploy/caddy/Caddyfile` | new |
| `deploy/README.md` | new — operator quickstart |
| `crates/agentos-kernel/src/config.rs` | env-var overlay |
| `crates/agentos-vault/src/lib.rs` | env-var password helper |
| `crates/agentos-api/src/handlers/system.rs` | add `/healthz`, `/readyz` |
| `crates/agentos-api/src/service.rs` | configurable bind addr, register health routes |
| `crates/agentos-cli/src/commands/serve.rs` | new — in-process boot |
| `crates/agentos-cli/src/main.rs` | dispatch `serve` |
| `.dockerignore` | new — exclude target, .git |

## Dependencies

- None (first phase).

## Test plan

- Unit: `Config::load` respects `AGENTOS__KERNEL__DATA_DIR` env var (serial_test because env is global).
- Unit: `vault_password_from_env_or_tty` reads env correctly.
- Integration: `curl http://localhost:8080/healthz` returns 200 during boot; `/readyz` 503 → 200 transition.
- Smoke: `docker compose up` → `curl https://$AGENTOS_DOMAIN/healthz` returns 200 after cert issuance.
- Clippy clean, fmt clean.

## Verification

```bash
cargo build --workspace --release
cargo test -p agentos-kernel -p agentos-vault -p agentos-api
cargo clippy --workspace -- -D warnings
docker build -t agentos:dev .
docker run --rm -e AGENTOS_VAULT_PASSWORD=test agentos:dev version
```

## Related

- [[Mobile App Plan]]
- [[02-mobile-oauth2-auth-layer]] — next phase, adds auth to this HTTPS surface
