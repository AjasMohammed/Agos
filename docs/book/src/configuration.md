# Configuration Reference

AgentOS is configured with a single TOML file. Three profiles ship in `config/`:

| Profile | Use | Paths |
|---------|-----|-------|
| `config/default.toml` | Development | `/tmp/agentos/...` (ephemeral; the kernel warns at startup) |
| `config/production.toml` | systemd / bare host | `/var/lib/agentos/...`, bus socket at `/run/agentos/` |
| `config/docker.toml` | Containers | everything under `/var/lib/agentos/data` (persistent volume) |

Select a profile with `--config` or the `AGENTOS_CONFIG` environment variable:

```bash
agentos --config config/production.toml start
```

Read and write values without hand-editing TOML (comments/formatting are preserved):

```bash
agentos config get llm.primary
agentos config set logging.log_level debug
agentos config list
```

## Key sections

### `[kernel]`
Task limits and scheduler settings: `max_concurrent_tasks`, `default_task_timeout_secs`,
`context_window_token_budget`, `health_port` (default `9091`), and `state_db_path`.
`sandbox_policy` is `trust_aware` (Core tools in-process, Community/Verified sandboxed),
`always`, or `never` (dev only). Sub-tables include `[kernel.task_limits]`,
`[kernel.tool_calls]`, `[kernel.autonomous_mode]`, `[kernel.events]`,
`[kernel.context_compaction]`, and `[kernel.tool_execution]`.

### `[secrets]`
`vault_path` — the AES-256-GCM encrypted vault. The passphrase is sourced in priority order:
1. `AGENTOS_VAULT_PASSPHRASE` (env var; systemd `EnvironmentFile`, mode `0600`)
2. `AGENTOS_VAULT_PASSPHRASE_FILE` (path to a secret file; Docker/K8s mounts)
3. interactive prompt (dev / first-run only)

Never combine `AGENTOS_AUTO_INIT_VAULT=true` with a baked-in default passphrase in
production. See [Security](./security.md).

### `[audit]`
`log_path`, `max_audit_entries` (0 = unlimited; older rows pruned on the 10-minute sweep),
`verify_last_n_entries` (chain verification depth on startup).

### `[tools]`
`core_tools_dir`, `user_tools_dir`, `data_dir`. `[tools.workspace] allowed_paths` lists
directories the agent may access beyond `data_dir` (system roots are rejected at startup).
`[tools.host_package]` (host package install, disabled by default) and `[env]` (per-agent
workspace package install with curated allowlists) live here too.

### `[ollama]` and `[llm]`
Provider endpoints and limits: `host`/`default_model`/`request_timeout_secs` for Ollama;
`openai_base_url`, `anthropic_base_url`, `gemini_base_url`, `max_tokens`,
`ollama_context_window`, and an optional `[[llm.fallback_models]]` chain. Runtime overrides:
`AGENTOS_OLLAMA_HOST`, `AGENTOS_LLM_URL`, `AGENTOS_OPENAI_BASE_URL`.

### `[memory]` / `[context_budget]` / `[scratchpad]`
Memory extraction and consolidation triggers, the context token budget split, and the agent
scratchpad. See [Memory](./memory.md).

### `[logging]`
`log_dir`, `log_level` (`trace`…`error`), `log_format` (`text` for dev, `json` for
production). Overridable at runtime with `RUST_LOG` or `agentos log set-level`.

### `[preflight]`
Boot pre-flight checks that fail fast before subsystem init: `min_free_disk_mb` and
`check_db_writable`.

### `[otel]`
OpenTelemetry export (opt-in; build with `--features otel`): `enabled`, `endpoint`,
`protocol`, `service_name`, `sample_rate`, and tool-input/output scrubbing. See
[Observability](./deploy/observability.md).

### `[health_monitor]`
`enabled`, `check_interval_secs` (default 30 — drives the systemd watchdog cadence), and
`[health_monitor.thresholds]` for CPU/memory/disk/GPU warnings.

### `[api]`
The REST API server: `enabled`, `host`, `port`, `docs_enabled` (the `/api/v1/docs` UI),
`operator_token`, `cors_allowed_origins`, `refresh_enabled`.

### `[approval]`
Tool-call approval mode: `auto`, `ask_edit` (default — approve reads, prompt for
writes/exec/control-plane), `ask_always`, or `deny`. Per-agent overrides go in
`[approval.agent_overrides]`. See [Security](./security.md).

### `[gateway]`
Bot mode. `enabled = false` by default; one `[[gateway.channels]]` table per bot, each
referencing a vault `credential_key` (never an inline token). See
[Gateway-first](./deploy/gateway.md).

### `[mcp]`
External MCP servers to connect at boot (`[[mcp.servers]]`). Imported tools register as
`TrustTier::Community` and are subject to full capability/permission enforcement. See
[MCP Integrations](./mcp.md).

Additional sections include `[notifications]`, `[skills]`, `[registry]`, `[runtime]`
(container backend + allowed images), and `[user_adaptation]`.
