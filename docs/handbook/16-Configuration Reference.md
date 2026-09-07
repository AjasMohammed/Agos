---
title: Configuration Reference
tags:
  - reference
  - configuration
  - v3
date: 2026-03-17
status: complete
---

# Configuration Reference

> Complete reference for every configuration key in AgentOS. Two config files exist: `config/default.toml` (development) and `config/production.toml` (deployment). Production values override development defaults.

---

## Overview

Configuration is loaded at kernel startup from one of:

- `config/default.toml` — development defaults, uses `/tmp/agentos/` paths
- `config/production.toml` — production values, uses `/var/lib/agentos/` paths

The active config file is selected at startup (typically by the `--config` flag or the `AGENTOS_CONFIG` environment variable). Environment variables take precedence over config file values for LLM provider URLs.

---

## `[kernel]`

Core kernel operational limits.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `max_concurrent_tasks` | integer | `4` | `8` | Maximum number of tasks running concurrently in the scheduler |
| `default_task_timeout_secs` | integer | `3600` | `3600` | Seconds before a running task is timed out if it has not completed |
| `context_window_max_entries` | integer | `500` | `500` | Maximum number of entries retained in a task's context window |
| `context_window_token_budget` | integer | `32000` | `32000` | Token budget for a single context window before eviction |
| `health_port` | integer | _(absent)_ | `9091` | HTTP port for the health check endpoint (production only) |
| `state_db_path` | string | `/tmp/agentos/data/kernel_state.db` | `/var/lib/agentos/data/kernel_state.db` | SQLite DB for persisted runtime state (tasks, escalations, cost snapshots) |
| `sandbox_policy` | string | `trust_aware` | `trust_aware` | Sandbox enforcement mode: `trust_aware` (Core tools in-process, Community/Verified sandboxed), `always` (all sandbox-eligible tools sandboxed), `never` (no sandboxing — development only, NOT for production) |
| `max_concurrent_sandbox_children` | integer | number of CPUs (min 2) | number of CPUs (min 2) | Maximum concurrent sandbox child processes. Defaults to the number of logical CPUs (minimum 2). Increase when running many Community/Verified tools in parallel. |

---

## `[kernel.context_compaction]`

In-task context compaction. The compactor periodically merges older context entries into a rolling `[ROLLING TASK SUMMARY]` block to keep the working window small during long agentic runs.

| Key | Type | Default | Description |
|---|---|---|---|
| `cadence` | integer | `4` | Fire the compactor every N completed iterations (and only when enough compactable entries exist). Lower values compact more aggressively. |
| `keep_recent_iterations` | integer | `2` | How many recent iterations' worth of entries to keep verbatim. Older entries are merged into the rolling summary. |
| `enable_llm_summarization` | bool | `true` | When true, the compactor calls the agent's LLM for a coherent semantic summary, falling back to the extractive heuristic on any LLM error. Set false for tight-latency agents or unreliable local models. |

---

## `[kernel.task_limits]`

Per-task iteration caps by complexity tier. Apply to normal (non-autonomous) tasks only.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_iterations_low` | integer | `50` | Max LLM inference iterations for low-complexity tasks |
| `max_iterations_medium` | integer | `200` | Max iterations for medium-complexity tasks |
| `max_iterations_high` | integer | `1000` | Max iterations for high-complexity tasks (must be > 0) |

Validation: `low <= medium <= high` and `high > 0`. At runtime, the actual limit is `max(resolved_limit, 1)`.

Autonomous tasks ignore these limits entirely — see `[kernel.autonomous_mode]` below.

---

## `[kernel.tool_calls]`

Parallel tool execution configuration for normal (non-autonomous) tasks.

| Key | Type | Default | Description |
|---|---|---|---|
| `allow_parallel` | bool | `true` | Allow agents to issue multiple tool calls per LLM turn |
| `max_parallel` | integer | `10` | Maximum concurrent tool calls per turn |

Autonomous tasks use `[kernel.autonomous_mode].max_parallel_tool_calls` instead.

---

## `[kernel.events]`

Event dispatch channel configuration.

| Key | Type | Default | Description |
|---|---|---|---|
| `channel_capacity` | integer | `1024` | Capacity of the internal event broadcast channel. Events dropped when full (with warning logged). Must be > 0. |

---

## `[kernel.tool_execution]`

Tool output and timeout limits for normal (non-autonomous) tasks.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_output_bytes` | integer | `262144` (256 KiB) | Maximum serialized bytes for a single tool's output. Truncated with marker if exceeded. Applies to all tasks including autonomous. |
| `default_timeout_seconds` | integer | `300` | Timeout for in-process (non-sandboxed) tool calls. Sandboxed tools use their manifest's `sandbox.max_cpu_ms`. Autonomous tasks use `[kernel.autonomous_mode].tool_timeout_seconds` instead. |

---

## `[kernel.autonomous_mode]`

Limits applied when a task is submitted with `autonomous=true`. These replace the complexity-based iteration caps so long-running agents can work to natural completion without hitting artificial ceilings.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_iterations` | integer | `10000` | Maximum LLM inference iterations before the task loop terminates |
| `task_timeout_secs` | integer | `86400` (24 hours) | Wall-clock timeout for the entire task |
| `tool_timeout_seconds` | integer | `600` (10 minutes) | Timeout for in-process tool calls within autonomous tasks |
| `max_parallel_tool_calls` | integer | `10` | Maximum concurrent tool calls per LLM turn for autonomous tasks |

**Example:**

```toml
[kernel.autonomous_mode]
max_iterations = 10000
task_timeout_secs = 86400
tool_timeout_seconds = 600
max_parallel_tool_calls = 10
```

> **Note:** Child tasks delegated by an autonomous parent automatically inherit `autonomous=true`, so sub-agents in an orchestrated workflow are not capped by the normal tier limits.

---

## `[secrets]`

Vault database location.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `vault_path` | string | `/tmp/agentos/vault/secrets.db` | `/var/lib/agentos/vault/secrets.db` | Path to the AES-256-GCM encrypted secrets SQLite database |

> **Warning:** The dev default is under `/tmp`, which is world-listable. Production must use a private path. The kernel creates parent directories with `0o700` permissions at startup.

---

## `[audit]`

Audit log database settings.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `log_path` | string | `/tmp/agentos/data/audit.db` | `/var/lib/agentos/data/audit.db` | Path to the append-only SQLite audit log database |
| `max_audit_entries` | integer | `0` | `500000` | Maximum rows to retain (0 = unlimited). Older rows are pruned on each 10-minute sweep. |
| `verify_last_n_entries` | integer | `1000` | `1000` | Number of recent entries to verify hash chain integrity at startup (0 = full chain verification, may be slow for large logs) |

---

## `[tools]`

Tool loading paths and data directory.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `core_tools_dir` | string | `/tmp/agentos/tools/core` | `/var/lib/agentos/tools/core` | Directory containing distribution-provided core tool manifests |
| `user_tools_dir` | string | `/tmp/agentos/tools/user` | `/var/lib/agentos/tools/user` | Directory for user-installed tool manifests |
| `data_dir` | string | `/tmp/agentos/data` | `/var/lib/agentos/data` | General data directory used by tools and the kernel |

---

## `[skills]`

Skill discovery paths. Skills are SKILL.toml-defined bundles of prompts, tools, and context that extend agent behaviour.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `core_skills_dir` | string | `skills/core` | `skills/core` | Directory containing distribution-provided core skill manifests |
| `user_skills_dir` | string | `skills/user` | `skills/user` | Directory for user-installed skill manifests |

---

## `[tools.workspace]`

Additional directories agents can access beyond `data_dir`. Validated at startup.

| Key | Type | Default | Description |
|---|---|---|---|
| `allowed_paths` | array of strings | `["/media", "/run/media"]` | Absolute paths to project or shared directories. System roots (`/`, `/etc`, `/var`, `/root`, `/home`) are rejected. Each path must have at least one subdirectory component. |

---

## `[tools.host_package]`

Host OS package install tool (`host-package-install`). **Disabled by default** and gated by an operator-controlled allowlist. The tool runs *outside* the bwrap sandbox via `pkexec` or a setuid helper; every call requires explicit user approval (control-plane risk class) **and** the requested package must appear verbatim in `allowlist`.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Enable the host package install tool. Disabled by default. |
| `privilege_escalator` | string | `"auto"` | Escalation policy: `"auto"` (prefer pkexec, else none), `"pkexec"`, `"helper"` (invoke the setuid binary at `helper_path`), or `"none"` (disables the tool). |
| `helper_path` | string | `/usr/local/libexec/agentos-pkg-helper` | Path to the setuid helper binary used when `privilege_escalator = "helper"`. |
| `managers` | array of strings | `["apt-get", "dnf", "pacman", "zypper", "apk", "brew"]` | Package managers the tool may invoke. The first one found on `PATH` is used. |
| `allowlist` | array of strings | `["python3", "python3-pip", "python3-venv", "nodejs", "npm", "git", "curl", "ca-certificates", "podman", "docker.io"]` | Operator-controlled allowlist. The agent can install only packages whose names match an entry verbatim, even after the user approves the call. |

---

## `[env]`

Per-agent workspace package install (the Env Install Execution Bridge). Packages are installed into isolated per-agent workspaces at `{data_dir}/workspaces/{agent_id}/{name}/`, *not* the host. Network is permitted only for the install duration.

Policy values: `"curated"` (package must be on the allowlist below — default), `"open"` (any package; dev/lab use only), `"locked"` (installation disabled).

| Key | Type | Default | Description |
|---|---|---|---|
| `python_policy` | string | `"curated"` | Install policy for Python packages. |
| `nodejs_policy` | string | `"curated"` | Install policy for Node.js packages. |
| `rust_policy` | string | `"curated"` | Install policy for Rust crates. |
| `system_policy` | string | `"locked"` | Install policy for system packages (host packages go through `host-package-install` instead). |
| `default_quota_bytes` | integer | `2147483648` (2 GiB) | Per-workspace size quota (informational, not enforced). |
| `install_timeout_secs` | integer | `120` | Timeout for a single install operation. |
| `python_allowlist` | array of strings | _(curated list)_ | Allowed Python packages under the `curated` policy (e.g. `flask`, `requests`, `pytest`, `numpy`, `pandas`, `fastapi`, `pydantic`). |
| `nodejs_allowlist` | array of strings | _(curated list)_ | Allowed Node.js packages under the `curated` policy (e.g. `express`, `jest`, `axios`, `lodash`, `zod`). |
| `rust_allowlist` | array of strings | _(curated list)_ | Allowed Rust crates under the `curated` policy (e.g. `serde`, `tokio`, `anyhow`, `clap`, `tracing`). |

---

## `[bus]`

Unix domain socket IPC configuration.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `socket_path` | string | `/tmp/agentos/agentos.sock` | `/run/agentos/agentos.sock` | Path to the Unix domain socket used for CLI-to-kernel communication |

---

## `[ollama]`

Ollama local LLM server settings.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `host` | string | `http://localhost:11434` | `http://ollama.service.consul:11434` | Base URL of the Ollama server. Override with `AGENTOS_OLLAMA_HOST`. |
| `default_model` | string | `llama3.2` | `llama3.2` | Default model name used when none is specified at agent connect time |
| `request_timeout_secs` | integer | `300` | `300` | HTTP request timeout for Ollama inference calls (seconds). Cloud-proxied models and large local models with many tools may need 300-600s. |

---

## `[llm]`

Remote LLM provider base URLs.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `custom_base_url` | string | _(commented out)_ | `https://llm-gateway.internal/v1` | Base URL for the custom OpenAI-compatible provider. Override with `AGENTOS_LLM_URL`. |
| `openai_base_url` | string | `https://api.openai.com/v1` | `https://api.openai.com/v1` | OpenAI API base URL. Override with `AGENTOS_OPENAI_BASE_URL`. |
| `anthropic_base_url` | string | `https://api.anthropic.com/v1` | `https://api.anthropic.com/v1` | Anthropic API base URL. |
| `gemini_base_url` | string | `https://generativelanguage.googleapis.com/v1beta` | `https://generativelanguage.googleapis.com/v1beta` | Google Gemini API base URL. |
| `max_tokens` | integer | `8192` | `8192` | Maximum output tokens for Anthropic requests. Claude 3 supports up to 8192, Claude 3.5 up to 16384. |
| `ollama_context_window` | integer | `32768` | `32768` | Context window size passed to Ollama as `num_ctx`. Set to match your model's actual context size. |

---

## `[memory]`

Embedding model cache location.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `model_cache_dir` | string | `models` | `/var/lib/agentos/data/models` | Directory where embedding model weights are cached |

---

## `[memory.extraction]`

Automatic fact extraction from task results.

| Key | Type | Default | Description | Valid Values |
|---|---|---|---|---|
| `enabled` | bool | `true` | Enable automatic memory extraction after task completion | `true` / `false` |
| `conflict_threshold` | float | `0.85` | Cosine similarity threshold above which two facts are considered conflicting | 0.0–1.0 |
| `max_facts_per_result` | integer | `5` | Maximum number of facts to extract from a single task result | ≥ 1 |
| `min_result_length` | integer | `50` | Minimum character length of a result before extraction is attempted | ≥ 0 |

---

## `[memory.consolidation]`

Background memory consolidation (episodic → semantic promotion).

| Key | Type | Default | Description | Valid Values |
|---|---|---|---|---|
| `enabled` | bool | `true` | Enable background consolidation loop | `true` / `false` |
| `min_pattern_occurrences` | integer | `3` | Minimum times a pattern must appear in episodes before it is promoted to semantic memory | ≥ 1 |
| `task_completions_trigger` | integer | `100` | Number of task completions that trigger a consolidation cycle | ≥ 1 |
| `time_trigger_hours` | integer | `24` | Hours between time-based consolidation cycles | ≥ 1 |
| `max_episodes_per_cycle` | integer | `500` | Maximum episodes to process in a single consolidation cycle | ≥ 1 |

---

## `[context_budget]`

Token allocation across context window partitions.

All `_pct` values are fractions of `total_tokens`. They do not need to sum to 1.0 — they define maximum allocations per partition; the kernel enforces `reserve_pct` as a hard floor before distributing the rest.

| Key | Type | Default | Description |
|---|---|---|---|
| `total_tokens` | integer | `128000` | Total token budget for the context window |
| `reserve_pct` | float | `0.25` | Fraction reserved for model output (not available to context partitions) |
| `system_pct` | float | `0.15` | Fraction allocated to system prompt entries |
| `tools_pct` | float | `0.18` | Fraction allocated to tool manifests and tool result entries |
| `knowledge_pct` | float | `0.30` | Fraction allocated to memory / knowledge entries |
| `history_pct` | float | `0.25` | Fraction allocated to conversation history entries |
| `task_pct` | float | `0.12` | Fraction allocated to task-specific context entries |
| `chars_per_token` | float | `4.0` | Characters-per-token ratio for token estimation. Default 4.0 is accurate for English/Latin text. Use 1.5–2.0 for CJK workloads. Clamped to [0.5, 16.0]. |

Validation: all `_pct` values must be >= 0.0, sum must be <= 1.001, `reserve_pct` must be in [0.0, 0.5], `chars_per_token` must be in [0.5, 16.0].

---

## `[health_monitor]`

System health monitoring thresholds (development config).

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Enable the background health monitor |
| `check_interval_secs` | integer | `30` | Seconds between health check sweeps |

### `[health_monitor.thresholds]`

| Key | Type | Default | Description |
|---|---|---|---|
| `cpu_warning_percent` | float | `85.0` | CPU usage percentage that triggers a warning |
| `memory_warning_percent` | float | `80.0` | Memory usage percentage that triggers a warning |
| `disk_warning_percent` | float | `85.0` | Disk usage percentage that triggers a warning |
| `disk_critical_percent` | float | `95.0` | Disk usage percentage that triggers a critical alert |
| `gpu_vram_warning_percent` | float | `90.0` | GPU VRAM usage percentage that triggers a warning |

---

## `[context]`

Context window summarization settings. Controls how the kernel compresses context entries when the token budget is exceeded.

| Key | Type | Default | Description |
|---|---|---|---|
| `summarization_mode` | string | `"llm"` | Context compression mode: `"llm"` (LLM-generated summaries, falls back to concat on failure), `"concat"` (concatenate entry snippets, legacy behavior), or `"off"` (entries silently evicted, no summary) |
| `summarization_max_input_chars` | integer | `8000` | Maximum characters of entry text sent to the summarizer LLM per compression event. Prevents sending enormous payloads on aggressive compression passes. |

---

## `[logging]`

Rolling file and stderr log configuration. Logs rotate daily with up to 7 days retained.

| Key | Type | Default | Description |
|---|---|---|---|
| `log_dir` | string | `"/tmp/agentos/logs"` | Directory where rolling log files are written. Set to `""` to disable file logging (stderr only). |
| `log_level` | string | `"info"` | Minimum log level: `trace`, `debug`, `info`, `warn`, `error`. Can be overridden at runtime with `RUST_LOG` or `agentos log set-level`. |
| `log_format` | string | `"text"` | Output format: `"text"` (human-readable) or `"json"` (structured, for log aggregators like Loki, Datadog, or Elasticsearch). Use `"json"` in production. |

---

## `[preflight]`

Boot pre-flight checks. These run before any subsystem starts so a misconfigured host fails fast with one precise diagnostic instead of crashing deep in init.

| Key | Type | Default | Description |
|---|---|---|---|
| `min_free_disk_mb` | integer | `512` | Minimum free disk (MB) on the data-dir partition required to boot. Set to `0` to disable the disk check. (Production profiles raise this.) |
| `check_db_writable` | bool | `true` | Probe that the audit/vault/state DB dirs, the log dir, and the bus-socket dir are writable before subsystem init. |

---

## `[otel]`

OpenTelemetry distributed tracing export.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Enable OpenTelemetry trace/metric export |
| `endpoint` | string | `"http://localhost:4317"` | OTLP collector endpoint URL |
| `protocol` | string | `"grpc"` | Export protocol: `"grpc"` or `"http"` |
| `service_name` | string | `"agentos"` | Service name attached to all exported traces and metrics |
| `sample_rate` | float | `1.0` | Trace sampling rate from 0.0 (no traces) to 1.0 (all traces) |
| `scrub_tool_inputs` | bool | `true` | Redact tool input payloads in trace spans (recommended for production to avoid leaking secrets) |
| `scrub_tool_outputs` | bool | `true` | Redact tool output payloads in trace spans |

---

## `[notifications]`

Agent notification inbox and delivery settings.

| Key | Type | Default | Description |
|---|---|---|---|
| `max_inbox_size` | integer | `1000` | Maximum messages stored per agent inbox. When reached, oldest read messages are purged on each write. |
| `notify_on_task_complete` | bool | `true` | Automatically notify user when a root task completes successfully |
| `notify_on_task_failed` | bool | `true` | Automatically notify user when a root task fails |

### `[notifications.adapters.webhook]`

HTTP webhook notification adapter for external integrations.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Enable webhook delivery |
| `url` | string | `""` | Webhook endpoint URL to POST notifications to |
| `secret` | string | `""` | HMAC-SHA256 secret for the `X-AgentOS-Signature` header (empty = no signature) |
| `min_priority` | string | `"warning"` | Minimum priority level to deliver: `info`, `warning`, `error`, `critical` |
| `max_retries` | integer | `3` | Maximum retry attempts on delivery failure |
| `retry_delay_secs` | integer | `5` | Seconds to wait between retry attempts |
| `timeout_secs` | integer | `10` | HTTP request timeout per delivery attempt |

### `[notifications.adapters.desktop]`

Native desktop notification adapter (libnotify / macOS Notification Center).

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Enable desktop notifications |
| `min_priority` | string | `"warning"` | Minimum priority level to display |
| `notify_on_task_complete` | bool | `true` | Show a desktop notification when a task completes |

### `[notifications.adapters.slack]`

Slack incoming webhook notification adapter.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Enable Slack delivery |
| `webhook_url` | string | `""` | Slack incoming webhook URL |
| `min_priority` | string | `"warning"` | Minimum priority level to deliver |
| `include_body` | bool | `true` | Include message body text in the Slack post |
| `max_retries` | integer | `3` | Maximum retry attempts on delivery failure |
| `retry_delay_secs` | integer | `2` | Seconds to wait between retry attempts |

---

## `[memory.context]`

Per-agent self-curated context memory. Injected into the context window at every task start, allowing agents to maintain persistent notes across tasks.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Enable per-agent context memory |
| `max_tokens` | integer | `4096` | Token budget for context memory content injected per task |
| `max_versions` | integer | `50` | Maximum version history entries to retain per agent |
| `db_path` | string | `"context_memory.db"` | SQLite database path (relative to `data_dir`) |
| `max_episodes_per_cycle` | integer | `500` | Maximum episodes to process in a single consolidation cycle |

---

## `[scratchpad]`

Agent scratchpad — a graph-aware knowledge store for agent working memory. Supports wikilink-connected pages with BFS traversal for automatic context injection.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Enable the agent scratchpad subsystem |
| `db_path` | string | `"scratchpad.db"` | SQLite database path (relative to `data_dir`) |
| `context_depth` | integer | `2` | BFS wikilink traversal depth for context injection (0 = seed page only) |
| `max_context_pages` | integer | `5` | Maximum pages injected into context per inference call |
| `max_context_bytes` | integer | `8192` | Maximum total bytes of scratchpad content injected per inference call |
| `max_page_size` | integer | `65536` (64 KiB) | Maximum content size per individual page (bytes) |
| `max_pages_per_agent` | integer | `1000` | Maximum pages an agent can create |
| `auto_write_on_completion` | bool | `true` | Auto-generate a scratchpad note when a task completes (success or failure) |
| `auto_write_min_steps` | integer | `3` | Minimum episodic entries for a task to qualify for auto-write (skips trivial tasks) |
| `auto_write_max_summary` | integer | `2048` | Maximum bytes for an auto-generated summary note |

---

## `[api]`

REST API and WebSocket server. When enabled, the kernel starts an HTTP API server alongside the Unix domain socket bus.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Start the API server at kernel boot. Disabled by default — must be explicitly enabled. |
| `host` | string | `"127.0.0.1"` | IP address to bind the API server. Use `"0.0.0.0"` to expose on all interfaces (use a reverse proxy in production). |
| `port` | integer | `8080` | TCP port for the API server. The WebSocket endpoint (`/api/v1/ws`) is served on the same port. |

**Example:**

```toml
[api]
enabled = true
host = "127.0.0.1"
port = 8080
```

**CORS:** The server automatically allows CORS from the configured `host:port`. Requests from other origins are rejected.

**Rate limiting:** 120-request burst, 2 requests/second per IP.

**TLS:** The API server does not terminate TLS. In production, place a TLS-terminating reverse proxy (nginx, Caddy) in front.

---

## `[registry]`

Tool registry marketplace configuration for `agentos tool search/add/publish`.

| Key | Type | Default | Description |
|---|---|---|---|
| `url` | string | `"https://registry.agentos.dev"` | Tool registry marketplace URL. Override with `AGENTOS_REGISTRY` environment variable for self-hosted registries. |

---

## `[mcp]` and `[[mcp.servers]]`

Model Context Protocol server configuration. Each entry spawns a child process via stdio JSON-RPC at kernel boot. Imported tools are registered with `TrustTier::Community` and subject to full AgentOS capability-token and `PermissionSet` enforcement.

| Key | Type | Default | Description |
|---|---|---|---|
| `servers` | array of tables | `[]` (empty) | MCP server definitions. Each entry has the keys below. |

Each `[[mcp.servers]]` entry:

| Key | Type | Description |
|---|---|---|
| `name` | string | Unique identifier for the MCP server (used in logs and tool prefixes) |
| `command` | string | Executable to spawn (e.g., `"npx"`, `"python"`, `"node"`) |
| `args` | array of strings | Command-line arguments passed to the spawned process |

**Example:**

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
```

---

## `[runtime]`

Container runtime for sandboxed code execution. Agents can launch containers only from the pre-approved `allowed_images` list.

| Key | Type | Default | Description |
|---|---|---|---|
| `backend` | string | `"docker"` | Container runtime backend: `"docker"` or `"none"` (disabled). |
| `default_memory_limit_mb` | integer | `1024` | Default per-container memory limit (MB). |
| `default_cpu_limit` | float | `1.0` | Default per-container CPU limit (cores). |
| `default_pids_limit` | integer | `100` | Default per-container PID limit. |
| `default_ttl_seconds` | integer | `3600` | Default container time-to-live before auto-removal. |
| `max_concurrent_containers` | integer | `10` | Maximum number of containers running concurrently. |
| `workspace_base_dir` | string | `/tmp/agentos/sandboxes` | Base directory for container workspace mounts. |
| `allowed_images` | array of strings | `["python:3.11-slim", "python:3.12-slim", "node:20-alpine", "node:22-alpine", "ubuntu:22.04", "ubuntu:24.04", "rust:1.78-slim", "alpine:3.19"]` | Pre-approved Docker images. Agents can only use images on this list. |

---

## `[user_adaptation]`

Deterministic post-task user preference proposal generator. Disabled by default; when enabled, proposals are queued for operator review rather than applied automatically.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Enable post-task user preference proposals. |
| `model` | string | `"compact"` | Model used to generate proposals. |
| `min_confidence` | float | `0.5` | Minimum confidence required to record a proposal. |
| `max_proposals_per_task` | integer | `3` | Maximum proposals generated per task. |
| `proposal_ttl_days` | integer | `30` | Pending proposals older than this are auto-expired (status `expired`, history preserved) by the TimeoutChecker sweep. |

---

## `[approval]`

Tool-call approval mode. Controls when the kernel auto-approves a tool call versus escalating it for human review. ControlPlane operations (kernel admin: key rotation, audit truncation, agent shutdown) always prompt regardless of mode.

| Key | Type | Default | Description |
|---|---|---|---|
| `mode` | string | `"ask_edit"` | Approval mode: `"auto"` (auto-approve everything except ControlPlane), `"ask_edit"` (default — auto-approve readonly tools; prompt for writes, exec, and control-plane), `"ask_always"` (prompt for everything except trivially-cheap ReadonlyScoped tools), `"deny"` (hard-deny anything that would otherwise prompt). |

### `[approval.agent_overrides]`

Per-agent approval mode overrides, keyed by agent display name. Each value is one of the `mode` values above.

```toml
[approval.agent_overrides]
research-bot = "auto"
writer-bot   = "ask_always"
```

---

## Complete `config/default.toml`

See `config/default.toml` in the repository for the complete, up-to-date configuration file with inline comments. The file includes all sections documented above.

---

## Complete `config/production.toml`

See `config/production.toml` in the repository for the current production profile.

---

## Related

- [[15-LLM Configuration]] — provider-specific configuration details
- [[09-Secrets and Vault]] — vault path and key derivation
- [[14-Audit Log]] — audit log path and retention
