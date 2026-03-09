---
title: Getting Started
tags: [guide, setup]
---

# Getting Started with AgentOS

## Prerequisites

- **Rust 1.75+** (install via [rustup](https://rustup.rs))
- **Linux** (seccomp-BPF sandboxing is Linux-only)
- **SQLite** (bundled with rusqlite, no external install needed)
- **Ollama** (optional, for local LLM inference)

## Build

```bash
# Debug build
cargo build --workspace

# Release build
cargo build --workspace --release

# Run tests
cargo test --workspace

# Lint
cargo clippy --workspace
```

## Boot the Kernel

```bash
# Start with default config
cargo run --bin agentctl -- start

# Start with custom config
cargo run --bin agentctl -- --config /path/to/config.toml start

# Start with vault passphrase (non-interactive)
cargo run --bin agentctl -- start --vault_passphrase "my-secret-passphrase"
```

### Boot Sequence

1. Parse configuration from TOML
2. Create data directories (audit, vault, tools, socket)
3. Install bundled core tool manifests
4. Open audit log (SQLite)
5. Initialize/open encrypted vault
6. Initialize capability engine (random 256-bit key)
7. Initialize HAL drivers
8. Load tool registry from core + user directories
9. Initialize tool runner with memory stores
10. Register WASM tools
11. Start remaining subsystems (scheduler, router, context manager)
12. Start Unix domain socket bus server
13. Begin accepting connections

## Connect an Agent

```bash
# Local Ollama
agentctl agent connect --provider ollama --model llama3.2 --name analyst

# OpenAI
agentctl agent connect --provider openai --model gpt-4 --name planner \
  --base_url https://api.openai.com

# Anthropic Claude
agentctl agent connect --provider anthropic --model claude-sonnet-4-20250514 --name writer

# Gemini
agentctl agent connect --provider gemini --model gemini-pro --name researcher
```

## Run a Task

```bash
# Run on specific agent
agentctl task run --agent analyst "Summarize the quarterly report data"

# Auto-route (kernel picks best agent)
agentctl task run "Parse the CSV file and extract key metrics"

# Check task status
agentctl task list
agentctl task logs <task-id>
```

## Example Session

```bash
# 1. Boot kernel
agentctl start

# 2. Connect agents
agentctl agent connect --provider ollama --model llama3.2 --name analyst
agentctl agent connect --provider ollama --model codellama --name coder

# 3. Set up secrets
agentctl secret set --scope global api_key sk-abc123

# 4. Grant permissions
agentctl perm grant analyst "network.outbound:rx"
agentctl perm grant coder "fs.user_data:rwx" "process.exec:x"

# 5. Create roles
agentctl role create researcher "Can read files and search memory"
agentctl perm grant researcher "fs.user_data:r" "memory.semantic:r"
agentctl role assign analyst researcher

# 6. Run tasks
agentctl task run --agent analyst "Research the latest data"
agentctl task run --agent coder "Write a parser for the output"

# 7. Agent communication
agentctl agent message analyst "Pass the parsed data to coder"

# 8. Check audit trail
agentctl audit logs --limit 20

# 9. System status
agentctl status
```

## Next Steps

- [[Architecture Overview]] - Understand the system design
- [[CLI Reference]] - Full command reference
- [[Configuration Guide]] - Customize your setup
- [[Tool System]] - Learn about built-in and custom tools
- [[Security Model]] - Understand the security layers
