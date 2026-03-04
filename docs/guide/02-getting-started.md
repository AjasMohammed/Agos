# Getting Started

This guide walks you through building, configuring, and running AgentOS from source.

---

## Prerequisites

| Requirement           | Minimum Version   | Notes                                             |
| --------------------- | ----------------- | ------------------------------------------------- |
| **Rust**              | 1.75+             | Install via [rustup](https://rustup.rs/)          |
| **Cargo**             | (ships with Rust) | Workspace build tool                              |
| **Linux**             | Any modern distro | Required for seccomp sandboxing                   |
| **SQLite**            | 3.x               | Bundled via `rusqlite` (no system install needed) |
| **Ollama** (optional) | Latest            | For local LLM inference                           |

### Optional (for cloud LLMs)

- **OpenAI API key** — for GPT-4o, etc.
- **Anthropic API key** — for Claude models
- **Google AI API key** — for Gemini models

---

## Building from Source

### 1. Clone the Repository

```bash
git clone https://github.com/agentos/agentos.git
cd agos
```

### 2. Build the Workspace

```bash
# Debug build (faster compilation)
cargo build --workspace

# Release build (optimized)
cargo build --workspace --release
```

### 3. Run Tests

```bash
# Run all unit tests
cargo test --workspace

# Run with verbose output
cargo test --workspace -- --nocapture

# Check code quality
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Configuration

AgentOS uses a TOML configuration file. The default config is at `config/default.toml`:

```toml
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 60
context_window_max_entries = 100

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
```

You can customize these paths for your environment. For production use, change `/tmp/agentos/` to a persistent directory like `/opt/agentos/`.

---

## Starting AgentOS

### Step 1: Boot the Kernel

```bash
# From the project root
cargo run --bin agentos-cli -- start
```

On first start, you will be prompted to set a vault passphrase:

```
Enter vault passphrase: ••••••••
🚀 Booting AgentOS kernel...
✅ Kernel started
   Bus: /tmp/agentos/agentos.sock
   Tools: 6 loaded

AgentOS is running. Use another terminal to run agentctl commands.
Press Ctrl+C to shutdown.
```

> **Important:** Remember your vault passphrase. It encrypts all secrets (API keys, tokens). You will need it every time you start the kernel.

You can also provide the passphrase non-interactively:

```bash
cargo run --bin agentos-cli -- start --vault-passphrase "your-passphrase"
```

### Step 2: Open a Second Terminal

All `agentctl` commands communicate with the running kernel over a Unix domain socket. Open a new terminal window for commands.

### Step 3: Check System Status

```bash
cargo run --bin agentos-cli -- status
```

This shows uptime, connected agents, active tasks, installed tools, and audit log entries.

---

## Connecting Your First Agent

### Option A: Local Ollama (Recommended for Development)

First, ensure [Ollama](https://ollama.com/) is running:

```bash
ollama serve          # in a separate terminal
ollama pull llama3.2  # download a model
```

Then connect it as an agent:

```bash
cargo run --bin agentos-cli -- agent connect \
  --provider ollama \
  --model llama3.2 \
  --name analyst
```

### Option B: Cloud LLM (OpenAI)

```bash
cargo run --bin agentos-cli -- agent connect \
  --provider openai \
  --model gpt-4o \
  --name researcher
```

You will be prompted to enter your API key (hidden input). The key is encrypted and stored in the vault — it never appears in shell history, environment variables, or config files.

### Option C: Anthropic Claude

```bash
cargo run --bin agentos-cli -- agent connect \
  --provider anthropic \
  --model claude-sonnet-4-20250514 \
  --name coder
```

### Option D: Google Gemini

```bash
cargo run --bin agentos-cli -- agent connect \
  --provider gemini \
  --model gemini-1.5-pro \
  --name writer
```

### Verify Connected Agents

```bash
cargo run --bin agentos-cli -- agent list
```

---

## Running Your First Task

```bash
cargo run --bin agentos-cli -- task run \
  --agent analyst \
  "Summarize the purpose of AgentOS in three bullet points"
```

The kernel will:

1. Validate the agent exists and is online
2. Issue a scoped capability token for the task
3. Forward the prompt to the LLM adapter
4. The LLM may call tools (file-reader, memory-write, etc.)
5. Return the final result

### Without Specifying an Agent

If you don't specify `--agent`, the kernel uses the **task router** to automatically select the best available agent based on the configured routing strategy.

```bash
cargo run --bin agentos-cli -- task run "What time is it?"
```

---

## Quick Example Session

Here's a complete session demonstrating key AgentOS features:

```bash
# Terminal 1: Start the kernel
cargo run --bin agentos-cli -- start

# Terminal 2: Interact with AgentOS

# 1. Connect an agent
cargo run --bin agentos-cli -- agent connect --provider ollama --model llama3.2 --name analyst

# 2. List tools
cargo run --bin agentos-cli -- tool list

# 3. Grant file permissions to the agent
cargo run --bin agentos-cli -- perm grant analyst fs.user_data:rw

# 4. Run a task
cargo run --bin agentos-cli -- task run --agent analyst "Read the contents of example.txt"

# 5. Store a secret
cargo run --bin agentos-cli -- secret set MY_API_KEY

# 6. View audit logs
cargo run --bin agentos-cli -- audit logs --last 20

# 7. Check status
cargo run --bin agentos-cli -- status

# 8. Disconnect the agent
cargo run --bin agentos-cli -- agent disconnect <agent-id>
```

---

## Next Steps

- **[Architecture](03-architecture.md)** — Understand how all the pieces fit together
- **[CLI Reference](04-cli-reference.md)** — Full command reference for every `agentctl` command
- **[Tools Guide](05-tools-guide.md)** — Learn about built-in tools and how to install more
- **[Security Model](06-security.md)** — Deep dive into secrets, permissions, and sandboxing
