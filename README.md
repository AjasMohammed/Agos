<p align="center">
  <h1 align="center">🧠 AgentOS</h1>
  <p align="center"><strong>A Minimalist, LLM-Native Operating System</strong></p>
  <p align="center"><em>An agentic operating environment built in Rust, designed ground-up for LLMs and AI agents — not for humans.</em></p>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?style=flat-square" alt="Language: Rust" />
  <img src="https://img.shields.io/badge/edition-2021-blue?style=flat-square" alt="Edition: 2021" />
  <img src="https://img.shields.io/badge/license-Apache--2.0-green?style=flat-square" alt="License: Apache-2.0" />
  <img src="https://img.shields.io/badge/status-Active%20Development-yellow?style=flat-square" alt="Status: Active Development" />
</p>

---

## What is AgentOS?

AgentOS is a purpose-built operating environment where **LLMs are the primary users**, not humans. Unlike traditional agent frameworks that wrap LLMs around existing operating systems, AgentOS is built from scratch around core principles:

- **🧠 LLMs are the CPU** — they process, reason, and decide
- **🔧 Tools are the programs** — installed, versioned, and sandboxed
- **📨 Intent is the syscall** — structured declarations replace raw function calls
- **🔒 Security is non-negotiable** — capability-based tokens, encrypted vault, seccomp
- **🤝 Agents are social** — every agent knows about other agents and can collaborate
- **🔌 Multi-LLM by default** — connect OpenAI, Anthropic, Ollama, Gemini simultaneously

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                        AgentOS                            │
│                                                          │
│  ┌──────────────┐                                        │
│  │  agentctl    │  CLI (clap)                            │
│  └──────┬───────┘                                        │
│         │  Unix Domain Socket                            │
│  ┌──────▼──────────────────────────────────────────────┐ │
│  │              Inference Kernel                        │ │
│  │  Task Scheduler · Context Manager · Agent Registry  │ │
│  │  Capability Engine · Secrets Vault · Audit Log      │ │
│  │  Task Router · Message Bus · Schedule Manager       │ │
│  └──────┬──────────────────────┬───────────────────────┘ │
│  ┌──────▼──────────┐   ┌──────▼────────────────────────┐ │
│  │ LLM Adapters    │   │ Tool Registry + Sandbox       │ │
│  │ Ollama · OpenAI │   │ file-reader · memory-search   │ │
│  │ Anthropic       │   │ file-writer · memory-write    │ │
│  │ Gemini · Custom │   │ data-parser · shell-exec      │ │
│  └─────────────────┘   │ WASM Tools (Wasmtime)         │ │
│                         └───────────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

---

## Quick Start

### Prerequisites

- **Rust 1.75+** ([rustup.rs](https://rustup.rs/))
- **Linux** (seccomp sandboxing is Linux-only)
- **Ollama** (optional, for local LLM inference) — [ollama.com](https://ollama.com/)

### Build

```bash
git clone https://github.com/agentos/agentos.git
cd agos
cargo build --workspace
```

### Run

```bash
# Terminal 1: Start the kernel
cargo run --bin agentos-cli -- start

# Terminal 2: Connect an agent and run a task
cargo run --bin agentos-cli -- agent connect --provider ollama --model llama3.2 --name analyst
cargo run --bin agentos-cli -- perm grant analyst fs.user_data:rw
cargo run --bin agentos-cli -- task run --agent analyst "Hello, AgentOS!"
```

### Run Tests

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Project Structure

```
agos/
├── crates/
│   ├── agentos-types/       # Shared types: IDs, intents, tokens, errors
│   ├── agentos-audit/       # Append-only immutable audit log (SQLite)
│   ├── agentos-vault/       # Encrypted secrets vault (AES-256-GCM + Argon2id)
│   ├── agentos-capability/  # HMAC-signed capability tokens + permission engine
│   ├── agentos-bus/         # Unix domain socket IPC layer
│   ├── agentos-llm/         # LLM adapters (Ollama, OpenAI, Anthropic, Gemini, Custom)
│   ├── agentos-tools/       # Built-in tool implementations
│   ├── agentos-wasm/        # WASM tool executor (Wasmtime runtime)
│   ├── agentos-sandbox/     # Seccomp-BPF sandboxed process execution
│   ├── agentos-kernel/      # Central orchestrator (scheduler, router, registry)
│   └── agentos-cli/         # agentctl CLI
├── config/
│   └── default.toml         # Default kernel configuration
├── tools/
│   └── core/                # Built-in tool manifests (.toml)
├── docs/
│   └── guide/               # User guide and documentation
├── v1-plans/                # Phase 1 implementation plans
├── v2-plans/                # Phase 2 implementation plans
└── Cargo.toml               # Workspace manifest
```

---

## Features

### ✅ Implemented (V1 + V2)

| Feature                 | Description                                                                                    |
| ----------------------- | ---------------------------------------------------------------------------------------------- |
| **Inference Kernel**    | Task scheduler, context manager, command router                                                |
| **Multi-LLM Support**   | Ollama, OpenAI, Anthropic, Gemini, Custom adapters                                             |
| **Task Routing**        | Capability-first, cost-first, latency-first, round-robin                                       |
| **8 Built-in Tools**    | file-reader/writer, memory-search/write, data-parser, shell-exec, agent-message, task-delegate |
| **WASM Tool Support**   | Custom tools in any language compiled to `.wasm`, installed at runtime via manifest            |
| **Capability Tokens**   | HMAC-SHA256 signed, unforgeable, scoped capability tokens                                      |
| **Permission System**   | Linux-style rwx permissions per resource class per agent                                       |
| **Encrypted Vault**     | AES-256-GCM encrypted secrets with Argon2id key derivation                                     |
| **Audit Log**           | Append-only SQLite log for every operation                                                     |
| **Agent Communication** | Direct messaging, task delegation, broadcast                                                   |
| **RBAC**                | Role-based access control with persistent roles                                                |
| **Background Tasks**    | agentd supervisor, cron/schedule management                                                    |
| **Seccomp Sandbox**     | BPF syscall filtering for tool execution                                                       |
| **Full CLI**            | agentctl with 9 command groups                                                                 |

### 🔮 Planned (Phase 3+)

| Feature                      | Target  |
| ---------------------------- | ------- |
| Web UI (Axum + HTMX)         | Phase 3 |
| Hardware Abstraction Layer   | Phase 3 |
| GPU Resource Manager         | Phase 3 |
| Python / Node.js SDKs        | Phase 3 |
| Multi-Agent Pipelines        | Phase 3 |
| Docker Production Deployment | Phase 3 |

---

## CLI Commands

```bash
agentctl start                    # Boot the kernel
agentctl status                   # System status

agentctl agent connect/list/disconnect   # Manage LLM agents
agentctl task run/list/logs/cancel       # Run and manage tasks
agentctl tool list/install/remove        # Manage agent tools
agentctl secret set/list/rotate/revoke   # Manage encrypted secrets
agentctl perm grant/revoke/show          # Manage agent permissions
agentctl role create/assign/list/delete  # Manage RBAC roles
agentctl schedule create/list/pause/resume/delete  # Cron jobs
agentctl bg run/list/logs/kill           # Background tasks
agentctl audit logs                      # View audit trail
```

See the [CLI Reference](docs/guide/04-cli-reference.md) for full details.

---

## Documentation

The `docs/guide/` folder contains a comprehensive user guide:

| Document                                                 | Description                                       |
| -------------------------------------------------------- | ------------------------------------------------- |
| [01 — Introduction](docs/guide/01-introduction.md)       | Vision, philosophy, and current status            |
| [02 — Getting Started](docs/guide/02-getting-started.md) | Build, configure, and run AgentOS                 |
| [03 — Architecture](docs/guide/03-architecture.md)       | System design, crate graph, kernel boot sequence  |
| [04 — CLI Reference](docs/guide/04-cli-reference.md)     | Complete command reference                        |
| [05 — Tools Guide](docs/guide/05-tools-guide.md)         | Built-in tools, WASM tools, manifests, sandboxing |
| [06 — Security Model](docs/guide/06-security.md)         | Vault, tokens, permissions, audit logging         |
| [07 — Configuration](docs/guide/07-configuration.md)     | TOML config reference and logging                 |

---

## Tech Stack

| Component      | Technology                           |
| -------------- | ------------------------------------ |
| Language       | Rust 2021 Edition                    |
| Async Runtime  | Tokio (multi-threaded)               |
| Serialization  | serde + serde_json + toml            |
| Error Handling | thiserror + anyhow                   |
| Logging        | tracing + tracing-subscriber         |
| Database       | SQLite (rusqlite, bundled)           |
| Encryption     | AES-256-GCM + Argon2id + HMAC-SHA256 |
| CLI            | clap (derive)                        |
| HTTP Client    | reqwest (for LLM API adapters)       |
| IPC            | Unix domain sockets                  |
| WASM Runtime   | Wasmtime 38 (Cranelift JIT)          |
| Sandbox        | seccomp-BPF (Linux) + Wasmtime       |
| Key Zeroing    | zeroize crate                        |

---

## Security

- **No hardcoded secrets** — all credentials stored in encrypted vault
- **No env var secrets** — secrets never stored in environment variables
- **Capability-based access** — unforgeable HMAC-signed tokens for every operation
- **Permission-gated resources** — agents start with zero permissions
- **Sandboxed tools** — seccomp-BPF syscall filtering
- **Immutable audit trail** — append-only log of every action
- **Memory safety** — implemented entirely in Rust
- **Secret zeroing** — credentials zeroed from memory after use

---

## Contributing

1. Fork the repository
2. Create a feature branch: `git checkout -b feat/my-feature`
3. Make your changes and add tests
4. Run checks: `cargo test --workspace && cargo clippy --workspace -- -D warnings`
5. Submit a pull request

### Commit Convention

Use conventional commits: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`

---

## License

Licensed under the [Apache License 2.0](LICENSE).

---

<p align="center"><em>AgentOS — an operating system for the age of agents.</em></p>
