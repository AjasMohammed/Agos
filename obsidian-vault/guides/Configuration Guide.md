---
title: Configuration Guide
tags: [guide, config]
---

# Configuration Guide

AgentOS uses TOML configuration files. Default config is at `config/default.toml`.

## Custom Config Path

```bash
agentctl --config /path/to/custom.toml start
```

## Configuration Sections

### `[kernel]`

| Key | Default | Description |
|---|---|---|
| `max_concurrent_tasks` | `4` | Max tasks running simultaneously |
| `default_task_timeout_secs` | `60` | Task timeout in seconds |
| `context_window_max_entries` | `100` | Max entries in per-task context window |

### `[secrets]`

| Key | Default | Description |
|---|---|---|
| `vault_path` | `/tmp/agentos/vault/secrets.db` | Path to encrypted vault SQLite DB |

### `[audit]`

| Key | Default | Description |
|---|---|---|
| `log_path` | `/tmp/agentos/data/audit.db` | Path to audit log SQLite DB |

### `[tools]`

| Key | Default | Description |
|---|---|---|
| `core_tools_dir` | `/tmp/agentos/tools/core` | Directory for built-in tool manifests |
| `user_tools_dir` | `/tmp/agentos/tools/user` | Directory for user-installed tool manifests |
| `data_dir` | `/tmp/agentos/data` | Shared data directory for tool I/O |

### `[bus]`

| Key | Default | Description |
|---|---|---|
| `socket_path` | `/tmp/agentos/agentos.sock` | Unix domain socket path |

### `[ollama]`

| Key | Default | Description |
|---|---|---|
| `host` | `http://localhost:11434` | Ollama server URL |
| `default_model` | `llama3.2` | Default model for Ollama provider |

### `[memory]`

| Key | Default | Description |
|---|---|---|
| `model_cache_dir` | `models` | Directory for caching embedding models (MiniLM-L6-v2) |

## Production Example

```toml
[kernel]
max_concurrent_tasks = 8
default_task_timeout_secs = 300
context_window_max_entries = 200

[secrets]
vault_path = "/opt/agentos/vault/secrets.db"

[audit]
log_path = "/opt/agentos/data/audit.db"

[tools]
core_tools_dir = "/opt/agentos/tools/core"
user_tools_dir = "/opt/agentos/tools/user"
data_dir = "/opt/agentos/data"

[bus]
socket_path = "/run/agentos/agentos.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"

[memory]
model_cache_dir = "/opt/agentos/models"
```

## Environment Variables

> AgentOS deliberately does **not** use environment variables for configuration. All secrets go through the encrypted [[Vault and Secrets|vault]]. This is a security design decision.

## Logging

Control log verbosity via `RUST_LOG`:

```bash
RUST_LOG=info agentctl start        # Default
RUST_LOG=debug agentctl start       # Verbose
RUST_LOG=agentos_kernel=debug agentctl start  # Per-crate
```

Structured JSON logging via the `tracing` crate.
