# systemd (host)

Run the AgentOS kernel as a hardened system service. The unit file
`deploy/agentos.service` is the source of truth.

## Install

```bash
# 1. Place the binary
sudo install -m 0755 agentos /usr/local/bin/agentos

# 2. Create the service user and data dirs
sudo useradd --system --home /var/lib/agentos --shell /usr/sbin/nologin agentos
sudo mkdir -p /var/lib/agentos /var/log/agentos /etc/agentos
sudo chown -R agentos:agentos /var/lib/agentos /var/log/agentos

# 3. Install config + unit
sudo cp config/production.toml /etc/agentos/config.toml
sudo cp deploy/agentos.service /etc/systemd/system/agentos.service

# 4. Enable + start
sudo systemctl daemon-reload
sudo systemctl enable --now agentos
```

The unit runs `agentos web serve --host 0.0.0.0 --port 8080`.

## Vault passphrase

The vault encryption passphrase must be provided. Use an environment file (mode `0600`,
owned by `root:root`) referenced by the unit:

```bash
# /etc/agentos/env
AGENTOS_VAULT_PASSPHRASE=<your-secret>
```

Alternatively, use systemd credentials:

```ini
LoadCredential=vault-passphrase:/etc/agentos/vault-passphrase
```

and read it from `$CREDENTIALS_DIRECTORY/vault-passphrase` in a wrapper.

## What the hardened unit does

- **`Type=notify` with a watchdog** — the kernel calls `sd_notify("READY=1")` once started,
  pings `WATCHDOG=1` on every health-check cycle, and sends `STOPPING=1` on graceful
  shutdown. `WatchdogSec=90s` (3× the 30s health check interval) means systemd restarts the
  process if it crashes **or** hangs.
- **Restart budget** — `Restart=on-failure`, `RestartSec=5s`, capped at 5 restarts per 5
  minutes (`StartLimitBurst=5`).
- **Resource limits** — `MemoryMax=2G`, `CPUQuota=200%`, `TasksMax=256`,
  `LimitNOFILE=65536`.
- **Filesystem isolation** — `ProtectSystem=strict`, `ProtectHome=true`, `PrivateTmp=true`;
  the only writable paths are `ReadWritePaths=/var/lib/agentos /var/log/agentos` plus the
  tmpfs `RuntimeDirectory=agentos` (the bus socket lives at `/run/agentos/agentos.sock`).
- **Privilege reduction** — `NoNewPrivileges=true`, `CapabilityBoundingSet=` (all
  capabilities dropped), `PrivateDevices=true`, `ProtectKernel*`, `RestrictSUIDSGID`,
  `LockPersonality`, `UMask=0077`.
- **Syscall/namespace policy** — `SystemCallFilter=@system-service`,
  `RestrictNamespaces=true`, `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`.

## The bwrap caveat

The hardened defaults (`RestrictNamespaces=true` + `SystemCallFilter=@system-service`)
**block** the bwrap-based `script` / `shell-exec` tools. They fail closed with `EPERM` —
safe, but those tools will not run. To enable them under systemd, relax **both** directives
(validate on a real host first):

```ini
RestrictNamespaces=false
SystemCallFilter=@system-service @mount @sandbox
```

Leave the hardened defaults if you sandbox via WASM or containers instead of bwrap. See
[Security](../security.md) for the full discussion.

## Operate

```bash
sudo systemctl status agentos
journalctl -u agentos -f          # logs (JSON when log_format="json")
curl -sf http://localhost:9091/healthz
```

The gateway-bot mode has its own unit, `deploy/agentos-gateway.service` — see
[Gateway-first](./gateway.md).
