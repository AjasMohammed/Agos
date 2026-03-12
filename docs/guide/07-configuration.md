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

[secrets]
vault_path = "/tmp/agentos/vault/secrets.db"

[audit]
log_path = "/tmp/agentos/data/audit.db"

[tools]
core_tools_dir = "/tmp/agentos/tools/core"
user_tools_dir = "/tmp/agentos/tools/user"
data_dir = "/tmp/agentos/data"

[bus]
socket_path = "/tmp/agentos/agentos.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"

[llm]
openai_base_url = "https://api.openai.com/v1"
anthropic_base_url = "https://api.anthropic.com/v1"
gemini_base_url = "https://generativelanguage.googleapis.com/v1beta"

[memory]
model_cache_dir = "models"
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
