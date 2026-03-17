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
| `default_task_timeout_secs` | integer | `60` | `120` | Seconds before a running task is timed out if it has not completed |
| `context_window_max_entries` | integer | `100` | `200` | Maximum number of entries retained in a task's context window |
| `context_window_token_budget` | integer | `8000` | `16000` | Token budget for a single context window before eviction |
| `health_port` | integer | _(absent)_ | `9091` | HTTP port for the health check endpoint (production only) |

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

---

## `[tools]`

Tool loading paths and data directory.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `core_tools_dir` | string | `/tmp/agentos/tools/core` | `/var/lib/agentos/tools/core` | Directory containing distribution-provided core tool manifests |
| `user_tools_dir` | string | `/tmp/agentos/tools/user` | `/var/lib/agentos/tools/user` | Directory for user-installed tool manifests |
| `data_dir` | string | `/tmp/agentos/data` | `/var/lib/agentos/data` | General data directory used by tools and the kernel |

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

---

## `[llm]`

Remote LLM provider base URLs.

| Key | Type | Dev Default | Prod Default | Description |
|---|---|---|---|---|
| `custom_base_url` | string | _(commented out)_ | `https://llm-gateway.internal/v1` | Base URL for the custom OpenAI-compatible provider. Override with `AGENTOS_LLM_URL`. |
| `openai_base_url` | string | `https://api.openai.com/v1` | `https://api.openai.com/v1` | OpenAI API base URL. Override with `AGENTOS_OPENAI_BASE_URL`. |
| `anthropic_base_url` | string | `https://api.anthropic.com/v1` | `https://api.anthropic.com/v1` | Anthropic API base URL. |
| `gemini_base_url` | string | `https://generativelanguage.googleapis.com/v1beta` | `https://generativelanguage.googleapis.com/v1beta` | Google Gemini API base URL. |

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

## Complete `config/default.toml`

```toml
[kernel]
# Development defaults. Use config/production.toml for deployment.
max_concurrent_tasks = 4
default_task_timeout_secs = 60
context_window_max_entries = 100
context_window_token_budget = 8000

[secrets]
# Development default path (ephemeral on many systems).
# WARNING: /tmp is world-listable. For production use a private path such as
# ~/.agentos/vault/secrets.db or $XDG_DATA_HOME/agentos/vault/secrets.db.
# The kernel creates parent directories with 0o700 permissions at startup.
vault_path = "/tmp/agentos/vault/secrets.db"

[audit]
# Development default path (ephemeral on many systems).
log_path = "/tmp/agentos/data/audit.db"
# Maximum rows to retain (0 = unlimited). Older rows are pruned on each 10-minute sweep.
max_audit_entries = 0

[tools]
# Development default paths (ephemeral on many systems).
core_tools_dir = "/tmp/agentos/tools/core"
user_tools_dir = "/tmp/agentos/tools/user"
data_dir = "/tmp/agentos/data"

[bus]
# Development default socket path.
socket_path = "/tmp/agentos/agentos.sock"

[ollama]
# Development default host. Override with AGENTOS_OLLAMA_HOST or production config.
host = "http://localhost:11434"
default_model = "llama3.2"

[llm]
# Optional custom/OpenAI-compatible provider endpoint.
# For production, set AGENTOS_LLM_URL or configure this in config/production.toml.
# custom_base_url = "https://llm-gateway.example.com/v1"
# Optional OpenAI base URL override (for gateways/proxies).
openai_base_url = "https://api.openai.com/v1"
# Reserved for provider endpoint documentation parity.
anthropic_base_url = "https://api.anthropic.com/v1"
gemini_base_url = "https://generativelanguage.googleapis.com/v1beta"

[memory]
model_cache_dir = "models"

[memory.extraction]
enabled = true
conflict_threshold = 0.85
max_facts_per_result = 5
min_result_length = 50

[memory.consolidation]
enabled = true
min_pattern_occurrences = 3
task_completions_trigger = 100
time_trigger_hours = 24
max_episodes_per_cycle = 500

[context_budget]
total_tokens = 128000
reserve_pct = 0.25
system_pct = 0.15
tools_pct = 0.18
knowledge_pct = 0.30
history_pct = 0.25
task_pct = 0.12

[health_monitor]
enabled = true
check_interval_secs = 30

[health_monitor.thresholds]
cpu_warning_percent = 85.0
memory_warning_percent = 80.0
disk_warning_percent = 85.0
disk_critical_percent = 95.0
gpu_vram_warning_percent = 90.0
```

---

## Complete `config/production.toml`

```toml
[kernel]
max_concurrent_tasks = 8
default_task_timeout_secs = 120
context_window_max_entries = 200
context_window_token_budget = 16000
health_port = 9091

[secrets]
vault_path = "/var/lib/agentos/vault/secrets.db"

[audit]
log_path = "/var/lib/agentos/data/audit.db"
# Retain at most 500,000 entries; older rows pruned on each 10-minute sweep.
max_audit_entries = 500000

[tools]
core_tools_dir = "/var/lib/agentos/tools/core"
user_tools_dir = "/var/lib/agentos/tools/user"
data_dir = "/var/lib/agentos/data"

[bus]
socket_path = "/run/agentos/agentos.sock"

[ollama]
# AGENTOS_OLLAMA_HOST overrides this when set.
host = "http://ollama.service.consul:11434"
default_model = "llama3.2"

[llm]
# AGENTOS_LLM_URL overrides this when set.
custom_base_url = "https://llm-gateway.internal/v1"
# AGENTOS_OPENAI_BASE_URL overrides this when set.
openai_base_url = "https://api.openai.com/v1"
anthropic_base_url = "https://api.anthropic.com/v1"
gemini_base_url = "https://generativelanguage.googleapis.com/v1beta"

[memory]
model_cache_dir = "/var/lib/agentos/data/models"

[context_budget]
total_tokens = 128000
reserve_pct = 0.25
system_pct = 0.15
tools_pct = 0.18
knowledge_pct = 0.30
history_pct = 0.25
task_pct = 0.12
```

> Note: `[memory.extraction]`, `[memory.consolidation]`, and `[health_monitor]` are not present in `production.toml` and fall back to development defaults.

---

## Related

- [[15-LLM Configuration]] — provider-specific configuration details
- [[09-Secrets and Vault]] — vault path and key derivation
- [[14-Audit Log]] — audit log path and retention
