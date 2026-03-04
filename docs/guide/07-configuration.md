# Configuration

AgentOS is configured via a single TOML file. The default configuration is at `config/default.toml`.

---

## Configuration File

```toml
[kernel]
max_concurrent_tasks = 4              # Max tasks running simultaneously
default_task_timeout_secs = 60        # Timeout for each task (seconds)
context_window_max_entries = 100      # Max entries per task's context window

[secrets]
vault_path = "/tmp/agentos/vault/secrets.db"  # Path to encrypted vault DB

[audit]
log_path = "/tmp/agentos/data/audit.db"       # Path to audit log DB

[tools]
core_tools_dir = "/tmp/agentos/tools/core"    # Built-in tool manifests
user_tools_dir = "/tmp/agentos/tools/user"    # User-installed tool manifests
data_dir = "/tmp/agentos/data"                # Working data directory for tools

[bus]
socket_path = "/tmp/agentos/agentos.sock"     # Unix domain socket for IPC

[ollama]
host = "http://localhost:11434"               # Ollama API endpoint
default_model = "llama3.2"                    # Default model for Ollama agents
```

---

## Section Reference

### `[kernel]`

| Key                          | Type    | Default | Description                                                                                  |
| ---------------------------- | ------- | ------- | -------------------------------------------------------------------------------------------- |
| `max_concurrent_tasks`       | integer | `4`     | Maximum number of tasks the scheduler will run simultaneously. Increase for more parallelism |
| `default_task_timeout_secs`  | integer | `60`    | Default timeout in seconds for each task. Tasks exceeding this are cancelled                 |
| `context_window_max_entries` | integer | `100`   | Maximum entries in a task's context window before old entries are evicted                    |

### `[secrets]`

| Key          | Type   | Default                         | Description                                                                                            |
| ------------ | ------ | ------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `vault_path` | string | `/tmp/agentos/vault/secrets.db` | Path to the encrypted SQLite vault database. The file and parent directories are created automatically |

### `[audit]`

| Key        | Type   | Default                      | Description                                       |
| ---------- | ------ | ---------------------------- | ------------------------------------------------- |
| `log_path` | string | `/tmp/agentos/data/audit.db` | Path to the append-only audit log SQLite database |

### `[tools]`

| Key              | Type   | Default                   | Description                                                               |
| ---------------- | ------ | ------------------------- | ------------------------------------------------------------------------- |
| `core_tools_dir` | string | `/tmp/agentos/tools/core` | Directory containing built-in tool TOML manifests                         |
| `user_tools_dir` | string | `/tmp/agentos/tools/user` | Directory for user-installed tool manifests                               |
| `data_dir`       | string | `/tmp/agentos/data`       | Working data directory. Tools read/write files relative to this directory |

### `[bus]`

| Key           | Type   | Default                     | Description                                  |
| ------------- | ------ | --------------------------- | -------------------------------------------- |
| `socket_path` | string | `/tmp/agentos/agentos.sock` | Unix domain socket path for CLI ↔ Kernel IPC |

### `[ollama]`

| Key             | Type   | Default                  | Description                          |
| --------------- | ------ | ------------------------ | ------------------------------------ |
| `host`          | string | `http://localhost:11434` | Ollama API endpoint URL              |
| `default_model` | string | `llama3.2`               | Default model name for Ollama agents |

---

## Custom Configuration

You can specify a custom config file with the `--config` flag:

```bash
agentctl --config /path/to/my-config.toml start
```

### Production Example

For production deployments, use persistent paths:

```toml
[kernel]
max_concurrent_tasks = 8
default_task_timeout_secs = 120
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
socket_path = "/var/run/agentos/agentos.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"
```

---

## Environment Variables

AgentOS does **not** use environment variables for configuration by design (security principle: secrets should never live in env vars). All configuration is loaded from the TOML file, and all secrets are stored in the encrypted vault.

---

## Logging

AgentOS uses the `tracing` crate for structured logging. By default, logging is configured at `INFO` level:

```
agentos=info
```

To enable debug logging, set the `RUST_LOG` environment variable:

```bash
RUST_LOG=agentos=debug cargo run --bin agentos-cli -- start
```

Log levels:

- `error` — Failures and critical issues
- `warn` — Recoverable problems
- `info` — Business events (task started, agent connected, tool executed)
- `debug` — Internal kernel details (for development only)
