---
title: Installation and First Run
tags:
  - docs
  - handbook
date: 2026-03-16
status: complete
---

# Installation and First Run

> Build AgentOS from source, configure the kernel, connect your first LLM agent, and run your first task.

---

## Prerequisites

| Requirement | Version | Notes |
|-------------|---------|-------|
| **Rust** | 1.75+ | Edition 2021. Install via [rustup](https://rustup.rs/) |
| **Cargo** | (bundled with Rust) | Workspace resolver v2 |
| **Linux** | x86_64 | Required for seccomp sandboxing. Other platforms build but skip sandbox features |
| **SQLite** | (bundled) | `rusqlite` compiles SQLite from source via the `bundled` feature |
| **Ollama** | (optional) | Local LLM inference at `http://localhost:11434`. Install from [ollama.ai](https://ollama.ai/) |
| **API Keys** | (optional) | OpenAI, Anthropic, or Google Gemini keys for cloud LLM providers |

---

## Building from Source

Clone the repository and build the entire workspace:

```bash
# Clone
git clone https://github.com/agentos/agentos.git
cd agentos

# Build all 16 workspace crates (debug mode)
cargo build --workspace

# Run all tests
cargo test --workspace

# Release build (optimized)
cargo build --workspace --release
```

The CLI binary is at `target/debug/agentctl` (or `target/release/agentctl` for release builds).

> [!tip] Lint and Format
> CI enforces these checks. Run them locally before committing:
> ```bash
> cargo clippy --workspace -- -D warnings
> cargo fmt --all -- --check
> ```

---

## Development Configuration

The default development configuration lives at `config/default.toml`. All paths use `/tmp/agentos/` for easy cleanup.

### `[kernel]` — Core kernel settings

| Key | Default | Description |
|-----|---------|-------------|
| `max_concurrent_tasks` | `4` | Maximum agent tasks running in parallel |
| `task_timeout_secs` | `60` | Seconds before a task is forcibly terminated |
| `max_context_entries` | `100` | Maximum entries per context window before eviction |
| `token_budget` | `8000` | Token budget per context window. Compress at 80%, checkpoint at 95% |

### `[vault]` — Encrypted secrets store

| Key | Default | Description |
|-----|---------|-------------|
| `db_path` | `"/tmp/agentos/vault.db"` | SQLite database for encrypted secrets |
| `argon2_mem_cost` | `65536` | Argon2id memory cost in KiB (64 MB) |
| `argon2_time_cost` | `3` | Argon2id iteration count |
| `argon2_parallelism` | `4` | Argon2id parallel lanes |

### `[audit]` — Append-only audit log

| Key | Default | Description |
|-----|---------|-------------|
| `db_path` | `"/tmp/agentos/audit.db"` | SQLite database for audit events |
| `max_entries` | `100000` | Maximum audit entries before pruning |
| `prune_sweep_interval_secs` | `600` | How often (seconds) the pruning sweep runs |

### `[tools]` — Tool discovery and execution

| Key | Default | Description |
|-----|---------|-------------|
| `core_dir` | `"tools/core"` | Directory containing built-in tool manifests |
| `user_dir` | `"tools/user"` | Directory for user-installed tool manifests |
| `sandbox_policy` | `"strict"` | Seccomp sandbox policy: `"strict"`, `"permissive"`, or `"disabled"` |

### `[bus]` — IPC socket

| Key | Default | Description |
|-----|---------|-------------|
| `socket_path` | `"/tmp/agentos/agentos.sock"` | Unix domain socket for CLI ↔ kernel communication |

### `[llm]` — LLM provider configuration

| Key | Default | Description |
|-----|---------|-------------|
| `ollama_host` | `"http://localhost:11434"` | Ollama API endpoint |
| `default_model` | `"llama3.2"` | Default model for Ollama inference |

### `[memory]` — Memory subsystem

| Key | Default | Description |
|-----|---------|-------------|
| `extraction_enabled` | `true` | Auto-extract memories from task results |
| `consolidation_interval_secs` | `3600` | How often (seconds) to consolidate episodic → semantic memory |

### `[context]` — Context window budget allocation

| Key | Default | Description |
|-----|---------|-------------|
| `system_prompt_pct` | `15` | Percentage of token budget reserved for system prompt |
| `tool_results_pct` | `18` | Percentage reserved for tool execution results |
| `knowledge_pct` | `30` | Percentage reserved for retrieved knowledge/memory |
| `history_pct` | `25` | Percentage reserved for conversation history |
| `task_pct` | `12` | Percentage reserved for current task context |

> [!note]
> Budget percentages sum to 100%. The context manager uses these to allocate the `token_budget` across entry types, compressing or evicting lower-priority entries when a category overflows.

### `[health]` — System health monitoring

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `true` | Enable periodic health checks |
| `interval_secs` | `30` | Health check interval |
| `cpu_threshold_pct` | `90` | CPU usage warning threshold |
| `memory_threshold_pct` | `85` | Memory usage warning threshold |
| `disk_threshold_pct` | `95` | Disk usage warning threshold |

---

## Production Configuration

Production configuration lives at `config/production.toml`. It uses persistent paths and higher limits.

### Key differences from development

| Setting | Development | Production |
|---------|------------|------------|
| `max_concurrent_tasks` | 4 | 8 |
| `task_timeout_secs` | 60 | 120 |
| `max_context_entries` | 100 | 200 |
| `token_budget` | 8000 | 16000 |
| `vault.db_path` | `/tmp/agentos/vault.db` | `/var/lib/agentos/vault.db` |
| `audit.db_path` | `/tmp/agentos/audit.db` | `/var/lib/agentos/audit.db` |
| `audit.max_entries` | 100,000 | 500,000 |
| `bus.socket_path` | `/tmp/agentos/agentos.sock` | `/run/agentos/agentos.sock` |
| `tools.core_dir` | `tools/core` | `/var/lib/agentos/tools/core` |
| `tools.user_dir` | `tools/user` | `/var/lib/agentos/tools/user` |
| `llm.ollama_host` | `http://localhost:11434` | `http://ollama.service.consul:11434` |

Production also adds:

| Key | Value | Description |
|-----|-------|-------------|
| `kernel.health_port` | `9091` | HTTP health check endpoint for load balancers |
| `llm.gateway_url` | `https://llm-gateway.internal/v1` | Centralized LLM gateway for cloud providers |

---

## Starting the Kernel

The kernel is started via the `agentctl start` command:

```bash
# Using default config (config/default.toml)
agentctl start

# Using a specific config file
agentctl --config config/production.toml start

# Providing vault passphrase via argument (not recommended for production)
agentctl start --vault-passphrase "my-secret-passphrase"

# Providing vault passphrase via environment variable
export AGENTOS_VAULT_PASSPHRASE="my-secret-passphrase"
agentctl start
```

### What happens during boot

When you run `agentctl start`, the kernel performs the following initialization sequence:

1. **Load configuration** from the specified TOML file
2. **Create directories** for audit, vault, tools, and bus socket
3. **Install core tool manifests** from the tools directory
4. **Open audit log** — initializes the SQLite database
5. **Open secrets vault** — decrypts with the provided passphrase (Argon2id key derivation)
6. **Initialize capability engine** — loads permission matrix
7. **Initialize HAL** — registers 6 hardware drivers (System, Process, Network, Sensor, GPU, Storage)
8. **Load tools** — reads manifests, validates trust tiers and signatures
9. **Build schema registry** — extracts JSON schemas from tool manifests
10. **Initialize memory stores** — episodic, semantic, and procedural stores with embedder
11. **Register WASM tools** — loads any WASM-based tools from manifests
12. **Initialize scheduler, context manager, agent registry, and router**
13. **Create pipeline engine** for multi-step workflow orchestration
14. **Start bus server** — listens on the Unix domain socket for CLI commands
15. **Initialize V3 subsystems** — cost tracker, escalation manager, injection scanner, risk classifier, snapshot manager, event bus
16. **Create IPC channels** — bounded channels (capacity 1024) for internal communication
17. **Emit `KernelStarted` audit event** — the system is now ready

After boot, the kernel enters its main event loop, running these subsystems as concurrent tasks:

- **Acceptor** — accepts new bus connections from `agentctl`
- **Executor** — processes pending agent tasks
- **TimeoutChecker** — sweeps expired tasks, escalations, snapshots, and resource locks
- **Scheduler** — triggers cron-scheduled jobs
- **EventDispatcher** — delivers events to subscribers
- **ToolLifecycleListener** — handles tool install/uninstall
- **CommNotificationListener** — handles agent-to-agent messaging
- **ScheduleNotificationListener** — handles schedule triggers
- **HealthMonitor** — periodic system health checks

Each subsystem auto-restarts on failure (up to 5 restarts per 60-second window).

### Vault passphrase

The vault passphrase is required to decrypt the secrets store. It is resolved in this order:

1. `--vault-passphrase` CLI argument
2. `AGENTOS_VAULT_PASSPHRASE` environment variable
3. Interactive prompt (recommended for production)

> [!warning]
> Never store the vault passphrase in plain text files or shell history. Use the interactive prompt or a secrets manager to inject the environment variable.

---

## Connecting Your First Agent

AgentOS supports multiple LLM providers simultaneously. Here are examples for each:

### Ollama (local, free)

```bash
# Start Ollama (separate terminal)
ollama serve

# Pull a model
ollama pull llama3.2

# Register an Ollama agent
agentctl agent register --name "local-agent" --provider ollama --model llama3.2
```

### OpenAI

```bash
# Store your API key in the vault
agentctl secret set openai-api-key

# Register an OpenAI agent
agentctl agent register --name "gpt-agent" --provider openai --model gpt-4
```

### Anthropic

```bash
# Store your API key in the vault
agentctl secret set anthropic-api-key

# Register an Anthropic agent
agentctl agent register --name "claude-agent" --provider anthropic --model claude-sonnet-4-20250514
```

### Google Gemini

```bash
# Store your API key in the vault
agentctl secret set gemini-api-key

# Register a Gemini agent
agentctl agent register --name "gemini-agent" --provider gemini --model gemini-pro
```

After registration, verify your agents are connected:

```bash
agentctl agent list
```

---

## Running Your First Task

With an agent connected, you can run a task:

```bash
# Run a simple task
agentctl task run --agent local-agent --prompt "List the files in the current directory"

# Run with a specific tool enabled
agentctl task run --agent local-agent --prompt "Read the contents of README.md" --tools file-reader

# Check task status
agentctl task list

# View task output
agentctl task show <task-id>
```

---

## Quick Example Session

Here is a complete end-to-end session from build to first task:

```bash
# 1. Build AgentOS
$ cd agentos
$ cargo build --workspace
   Compiling agentos-types v0.1.0
   Compiling agentos-audit v0.1.0
   ... (16 crates)
   Compiling agentos-cli v0.1.0
    Finished `dev` profile [unoptimized + debuginfo]

# 2. Start Ollama (separate terminal)
$ ollama serve
$ ollama pull llama3.2

# 3. Start the kernel
$ ./target/debug/agentctl start
Enter vault passphrase: ********
[INFO] Audit log initialized at /tmp/agentos/audit.db
[INFO] Secrets vault opened
[INFO] Loaded 8 core tools
[INFO] Bus server listening on /tmp/agentos/agentos.sock
[INFO] Kernel started successfully

# 4. Register an agent (separate terminal)
$ ./target/debug/agentctl agent register --name "my-agent" --provider ollama --model llama3.2
Agent registered: my-agent (id: a1b2c3d4-...)

# 5. Run a task
$ ./target/debug/agentctl task run --agent my-agent --prompt "What is 2 + 2?"
Task created: t5e6f7g8-...
Result: 2 + 2 = 4

# 6. Check system status
$ ./target/debug/agentctl status
Kernel: running
Agents: 1 online
Tasks: 1 completed, 0 pending
Uptime: 45s

# 7. View audit log
$ ./target/debug/agentctl audit list --limit 5
[2026-03-16T10:00:01Z] KernelStarted
[2026-03-16T10:00:15Z] AgentRegistered agent=my-agent
[2026-03-16T10:00:20Z] TaskCreated task=t5e6f7g8
[2026-03-16T10:00:21Z] InferenceCompleted agent=my-agent tokens=42
[2026-03-16T10:00:21Z] TaskCompleted task=t5e6f7g8
```

---

## Migration from Development to Production

When you are ready to move from development to production, follow this checklist:

### 1. Create production directories

```bash
sudo mkdir -p /var/lib/agentos/{vault,audit,tools/core,tools/user}
sudo mkdir -p /run/agentos
sudo chown agentos:agentos /var/lib/agentos /run/agentos
```

### 2. Copy tool manifests

```bash
sudo cp -r tools/core/* /var/lib/agentos/tools/core/
```

### 3. Switch configuration

```bash
# Use production config
agentctl --config config/production.toml start
```

### 4. Production checklist

- [ ] Create dedicated `agentos` system user and group
- [ ] Set directory ownership to `agentos:agentos`
- [ ] Copy `config/production.toml` and review all paths
- [ ] Set `AGENTOS_VAULT_PASSPHRASE` via secrets manager (not env file)
- [ ] Configure Ollama at the production endpoint (e.g., Consul service discovery)
- [ ] Set up the LLM gateway URL for cloud providers
- [ ] Verify `health_port = 9091` is accessible to your load balancer
- [ ] Set up log rotation for audit database
- [ ] Set `audit.max_entries` appropriate for your retention policy
- [ ] Build with `--release` for optimized binary
- [ ] Set up systemd service for automatic restarts

### 5. Systemd service example

```ini
[Unit]
Description=AgentOS Kernel
After=network.target

[Service]
Type=simple
User=agentos
Group=agentos
ExecStart=/usr/local/bin/agentctl --config /etc/agentos/production.toml start
Environment=AGENTOS_VAULT_PASSPHRASE_FILE=/run/secrets/agentos-vault
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

> [!tip] Docker
> For container deployments, see the Docker deployment plan in the project's `obsidian-vault/plans/` directory.
