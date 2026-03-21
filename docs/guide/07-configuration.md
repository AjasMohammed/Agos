# Configuration

AgentOS supports two profiles out of the box:

- `config/default.toml` for local development (includes `/tmp` defaults)
- `config/production.toml` for deployment (persistent paths + explicit LLM endpoints)

Use a custom profile with:

```bash
agentctl --config /path/to/config.toml start
```

---

## Default Configuration (Development)

`config/default.toml` intentionally uses development-friendly defaults. The kernel logs a startup warning when runtime paths point under `/tmp` or `/var/tmp`.

```toml
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 60
context_window_max_entries = 100
context_window_token_budget = 8000
# SQLite DB for persisted runtime state (tasks, escalations, cost snapshots).
state_db_path = "/tmp/agentos/data/kernel_state.db"

[kernel.task_limits]
# Per-task iteration caps by complexity tier (low/medium/high).
# Agents may override via task metadata up to the tier's max.
max_iterations_low = 10
max_iterations_medium = 25
max_iterations_high = 50

[kernel.tool_calls]
# Allow agents to issue multiple tool calls per LLM turn.
allow_parallel = true
max_parallel = 5

[kernel.events]
# Capacity of the internal event dispatch channel.
# Increase if you observe dropped events under heavy load.
channel_capacity = 1024

[kernel.tool_execution]
max_output_bytes = 262144    # 256 KiB — truncated with marker if exceeded
default_timeout_seconds = 60 # Timeout for in-process (non-sandboxed) tools

[secrets]
vault_path = "/tmp/agentos/vault/secrets.db"

[audit]
log_path = "/tmp/agentos/data/audit.db"
max_audit_entries = 0          # 0 = unlimited
verify_last_n_entries = 1000   # Hash chain entries to verify at boot (0 = full)

[tools]
core_tools_dir = "/tmp/agentos/tools/core"
user_tools_dir = "/tmp/agentos/tools/user"
data_dir = "/tmp/agentos/data"

[tools.workspace]
# Additional directories agents can access beyond data_dir.
# Must be absolute paths. System roots (/, /etc, /var, /root) are rejected.
allowed_paths = []

[bus]
socket_path = "/tmp/agentos/agentos.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"

[llm]
openai_base_url = "https://api.openai.com/v1"
anthropic_base_url = "https://api.anthropic.com/v1"
gemini_base_url = "https://generativelanguage.googleapis.com/v1beta"
max_tokens = 8192              # Max output tokens for Anthropic
ollama_context_window = 32768  # Context window size for Ollama

[memory]
model_cache_dir = "models"

[context_budget]
total_tokens = 128000
reserve_pct = 0.25
system_pct = 0.15
tools_pct = 0.18
knowledge_pct = 0.30
history_pct = 0.25
task_pct = 0.12
chars_per_token = 4.0  # Use 1.5-2.0 for CJK workloads

[health_monitor]
enabled = true
check_interval_secs = 30
```

---

## Production Baseline

Use `config/production.toml` for first deployment:

```toml
[secrets]
vault_path = "/var/lib/agentos/vault/secrets.db"

[audit]
log_path = "/var/lib/agentos/data/audit.db"

[tools]
core_tools_dir = "/var/lib/agentos/tools/core"
user_tools_dir = "/var/lib/agentos/tools/user"
data_dir = "/var/lib/agentos/data"

[bus]
socket_path = "/run/agentos/agentos.sock"

[ollama]
host = "http://ollama.service.consul:11434"
default_model = "llama3.2"

[llm]
custom_base_url = "https://llm-gateway.internal/v1"
openai_base_url = "https://api.openai.com/v1"
anthropic_base_url = "https://api.anthropic.com/v1"
gemini_base_url = "https://generativelanguage.googleapis.com/v1beta"

[memory]
model_cache_dir = "/var/lib/agentos/data/models"
```

---

## Docker / Container Profile

Use `config/docker.toml` when running inside a Docker container (loaded automatically by the `Dockerfile`'s `CMD`):

```toml
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 300
context_window_max_entries = 200
context_window_token_budget = 128000
health_port = 9091

[bus]
socket_path = "/var/lib/agentos/data/agentos.sock"

[audit]
log_path = "/var/lib/agentos/data/audit.db"

[secrets]
vault_path = "/var/lib/agentos/data/vault.db"

[tools]
core_tools_dir = "/var/lib/agentos/tools/core"
user_tools_dir  = "/var/lib/agentos/tools/user"
data_dir        = "/var/lib/agentos/data"

[ollama]
host          = "http://ollama:11434"
default_model = "llama3.2"
```

> The snippet above shows the key sections. The full file (`config/docker.toml`) also includes `[llm]`, `[memory]`, `[memory.extraction]`, `[memory.consolidation]`, `[context_budget]`, and `[health_monitor]` sections.

Key differences from the development default:
- All runtime paths are under `/var/lib/agentos/` (backed by named Docker volumes — survives restarts)
- Ollama is reachable at `http://ollama:11434` (Docker service name, not `localhost`)
- Health monitor enabled; exposes `/healthz` on port `9091`
- `context_window_token_budget` raised to `128000` for larger models

### Environment Variables

The compose stack passes credentials via environment variables rather than baking them into the image.
Copy `.env.example` to `.env` and fill in the required values before starting:

```bash
cp .env.example .env
# edit .env — set AGENTOS_VAULT_PASSPHRASE to a strong random value:
#   openssl rand -base64 32
```

| Variable | Required | Description |
|---|---|---|
| `AGENTOS_VAULT_PASSPHRASE` | **Yes** | Derives the AES-256-GCM vault key. Never commit this value. |
| `AGENTOS_OLLAMA_HOST` | No | Override Ollama URL (default: `http://ollama:11434`) |
| `AGENTOS_LLM_URL` | No | Custom/OpenAI-compatible gateway base URL |
| `AGENTOS_OPENAI_BASE_URL` | No | Override OpenAI base URL |
| `RUST_LOG` | No | Tracing filter — `agentos=info` for production |

### Security settings in `docker-compose.yml`

The provided `docker-compose.yml` enables two important hardening options:

```yaml
read_only: true            # Root filesystem is read-only
security_opt:
  - no-new-privileges:true # Process cannot escalate privileges
tmpfs:
  - /tmp                   # Writable tmpfs for scratch space
  - /run                   # Writable tmpfs for runtime sockets
```

All persistent state lives in named volumes (`agentos-data`, `agentos-user-tools`), not in the container filesystem.

---

## LLM Endpoint Resolution

For agent connect flows, endpoint precedence is:

1. CLI `--base-url`
2. Environment variable override
3. Config file value
4. Provider default (only where supported)

Provider behavior:

- **Ollama:** `--base-url` -> `AGENTOS_OLLAMA_HOST` -> `[ollama].host`
- **Custom provider:** `--base-url` -> `AGENTOS_LLM_URL` -> `[llm].custom_base_url` (required; no localhost fallback)
- **OpenAI:** `--base-url` -> `AGENTOS_OPENAI_BASE_URL` -> `[llm].openai_base_url` -> official OpenAI endpoint

`anthropic_base_url` and `gemini_base_url` are recorded in config for deployment documentation parity.

---

## Environment Variables

Environment variables are supported only for non-secret endpoint overrides:

- `AGENTOS_OLLAMA_HOST`
- `AGENTOS_LLM_URL`
- `AGENTOS_OPENAI_BASE_URL`
- `RUST_LOG`

Secrets (API keys, tokens, credentials) must stay in the encrypted vault.

---

## Migration Checklist (Old Paths -> Production Layout)

1. Stop AgentOS services.
2. Create runtime directories:
   - `/var/lib/agentos/vault`
   - `/var/lib/agentos/data`
   - `/var/lib/agentos/tools/core`
   - `/var/lib/agentos/tools/user`
   - `/run/agentos`
3. Copy existing state from `/tmp/agentos` if preserving local data:
   - `/tmp/agentos/vault/secrets.db` -> `/var/lib/agentos/vault/secrets.db`
   - `/tmp/agentos/data/audit.db` -> `/var/lib/agentos/data/audit.db`
   - `/tmp/agentos/tools/*` -> `/var/lib/agentos/tools/*`
4. Ensure service user ownership and permissions for `/var/lib/agentos` and `/run/agentos`.
5. Start with production profile:

```bash
agentctl --config config/production.toml start
```

6. Verify:

```bash
agentctl --config config/production.toml status
grep -n "localhost" config/production.toml
```

---

## Logging

AgentOS uses `tracing` for structured logs.

- Startup logs include configured LLM endpoint values (non-secret).
- Startup warnings are emitted when runtime paths are under temporary directories.

Enable debug logs:

```bash
RUST_LOG=agentos=debug cargo run --bin agentos-cli -- start --config config/production.toml
```
