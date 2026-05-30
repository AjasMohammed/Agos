# Docker & Compose

The `Dockerfile` produces a multi-stage image: a `rust:1.91` builder (OpenSSL statically
linked, mold linker) and a slim Debian runtime with bubblewrap and CA certificates. It runs
as a dedicated non-root user (`uid/gid 65532`), embeds the config and core tool/plugin
manifests, exposes ports `8080` (web/API) and `9091` (health/metrics), and ships a
`HEALTHCHECK` that runs `agentos healthz`.

## Compose stack

`docker-compose.yml` brings up three services: **agentos**, **jaeger** (traces), and
**ollama** (local inference).

```bash
# 1. Optionally set a vault passphrase
cp .env.example .env            # set AGENTOS_VAULT_PASSPHRASE for an explicit secret

# 2. Start the stack
docker compose up -d

# 3. Check health
curl -sf http://localhost:9091/healthz
```

The `agentos` service is hardened:

- **`read_only: true`** root filesystem, with `tmpfs` mounts for `/tmp`, `/run`, and
  `/var/log/agentos`.
- **`no-new-privileges:true`**.
- **Named volumes** `agentos-data` and `agentos-user-tools` for persistence.
- Mounts `config/docker.toml` read-only at `/etc/agentos/config.toml`.
- Runs `web serve --host 0.0.0.0 --port 8080`; publishes `8080` and `9091`.

Jaeger is wired automatically (`AGENTOS_OTEL_ENDPOINT=http://jaeger:4317`); open the UI at
<http://localhost:16686>.

> The Docker socket mount (`/var/run/docker.sock`) is present so the container-runtime tools
> (`container-create` / `container-exec` / `container-logs`) work. **This grants the
> container effective root on the host** — keep it commented out unless you need container
> tooling.

## Vault passphrase

With `AGENTOS_AUTO_INIT_VAULT=true` and no `AGENTOS_VAULT_PASSPHRASE`, AgentOS generates a
managed passphrase file next to the vault DB inside the persistent volume. That is a
convenience mode, not the strongest at-rest isolation — for production, supply an explicit
secret via `AGENTOS_VAULT_PASSPHRASE` or mount one with `AGENTOS_VAULT_PASSPHRASE_FILE`.

## Container config

`config/docker.toml` is tuned for the container runtime: all paths live under
`/var/lib/agentos/data` (the persistent volume), the vault is at
`/var/lib/agentos/data/vault.db`, `[logging] log_format = "json"` for `docker logs`, the API
binds `0.0.0.0:8080`, and Ollama points at the `ollama` service hostname.

## Observability overlay

Add Prometheus + Grafana with the overlay (see [Observability](./observability.md)):

```bash
docker compose -f docker-compose.yml \
               -f deploy/observability/docker-compose.observability.yml up -d
```

## Gateway overlay

To run the messaging-bot daemon instead, use the gateway overlay
(`docker-compose.gateway.yml`) described in [Gateway-first](./gateway.md).
