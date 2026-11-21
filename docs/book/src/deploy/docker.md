# Docker & Compose

The `Dockerfile` produces a multi-stage image: a `rust:1.98` builder (OpenSSL statically
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
- Runs `start`; the REST API binds per `[api]` in `config/docker.toml` (`0.0.0.0:8080`). Publishes `8080` and `9091` on host loopback.

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


## Back up and restore

Use a pinned image version. Keep the vault passphrase in a secret manager and
retain the deployment configuration and any mounts outside the Compose volumes
separately. Encrypt the backup because task inputs, outputs and logs can
contain sensitive business data.

Stop the daemon for a consistent snapshot across SQLite files and copy both
volumes together. These commands assume the default Compose volume mounts.

```bash
umask 077
docker compose stop agentos
docker compose run --rm --no-deps -T --user 0 --entrypoint tar agentos \
  --exclude=./data/agentos.sock -C /var/lib/agentos -cf - ./data ./tools/user \
  > agentos-state.tar
docker compose start agentos
```

Run the restore drill on a separate host with fresh Compose volumes, the pinned
image, the original configuration, and the same vault passphrase. Confirm the
archive and secret are available before starting.

```bash
docker compose run --rm --no-deps -T --user 0 --entrypoint tar agentos \
  -C /var/lib/agentos -xpf - < agentos-state.tar
docker compose up -d
curl -fsS http://127.0.0.1:9091/readyz
docker compose exec agentos agentos secret list
```

Confirm a known vault entry and representative task history. Keep external
credentials and channel polling disabled on the recovery host until the old
instance is offline and the new one is ready to take over. The release CI smoke
test boots the image and verifies that a known vault entry survives restoration
to a fresh volume.
