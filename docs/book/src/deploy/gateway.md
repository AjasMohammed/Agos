# Gateway-first (run as a bot)

Gateway mode runs AgentOS as a long-lived **messaging bot**. Instead of serving a web UI,
`agentos gateway run` boots the kernel and connects every channel declared in the
`[gateway]` config block (Telegram, Discord, Slack, Matrix, ntfy, webhook), then runs until
`SIGTERM`. Inbound chat messages become agent tasks; replies flow back over the same channel.

The boot and signal loop are shared with `agentos start`, so there is exactly one kernel-boot
code path.

## Configure channels

Channels are declared in the `[gateway]` block. **Tokens are never inline** — each channel
references a vault key via `credential_key`, which you seed with `agentos secret set`.

```toml
[gateway]
enabled = true

[[gateway.channels]]
kind = "telegram"                       # telegram | discord | slack | matrix | ntfy | webhook
display_name = "Ops Bot"
credential_key = "telegram_bot_token"   # vault key holding the token
active_agent = "assistant"              # default agent for inbound chat
# external_id omitted for Telegram → auto-discovered from the first /start
```

Add one `[[gateway.channels]]` table per bot.

## Seed the token

```bash
agentos secret set telegram_bot_token <your-bot-token>
```

The token is encrypted in the vault (AES-256-GCM) and resolved at boot by `credential_key`.

## Run it

```bash
agentos gateway run
```

## systemd

Use the dedicated unit `deploy/agentos-gateway.service`, which runs
`agentos gateway run` with the same hardening as the kernel unit (watchdog, resource limits,
read-only filesystem, dropped capabilities). The gateway needs outbound network for the
provider APIs (`AF_INET`/`AF_INET6` are allowed).

```bash
sudo cp deploy/agentos-gateway.service /etc/systemd/system/
sudo systemctl enable --now agentos-gateway
```

> The gateway unit and `agentos.service` **share the same bus socket and data dir**, so they
> are **mutually exclusive on one host** unless you give each a distinct `AGENTOS_CONFIG` and
> data directory. The bwrap caveat from [systemd](./systemd.md) applies here too.

## Docker

Use the overlay `docker-compose.gateway.yml`, which runs the `gateway run` command in a
read-only, capability-dropped container on a **separate volume** from the web service (so the
two daemons never share a bus socket or state DB):

```bash
docker compose -f docker-compose.yml -f docker-compose.gateway.yml up -d agentos-gateway
```

No web/health ports are published by default. Pre-seed the gateway's vault with channel
tokens, and supply the vault passphrase via a Docker/K8s secret
(`AGENTOS_VAULT_PASSPHRASE_FILE=/run/secrets/vault_pass`) in production.
