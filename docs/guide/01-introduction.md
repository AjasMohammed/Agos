# Introduction to AgentOS

> _An agentic operating environment built in Rust, designed ground-up for LLMs and AI agents — not for humans._

---

## What is AgentOS?

AgentOS is a **purpose-built operating environment** where **LLMs are the primary users**, not humans. Unlike traditional AI agent frameworks (LangChain, CrewAI, LangGraph) that wrap LLMs around existing operating systems, AgentOS is built from scratch around a radical idea:

- **LLMs are the CPU** — they process, reason, and decide
- **Tools are the programs** — installed, versioned, and sandboxed
- **Intent is the syscall** — structured declarations replace raw function calls
- **The kernel manages everything** — scheduling, memory, security, context
- **Agents are peers** — every connected agent is aware of and can collaborate with others
- **Secrets are first-class** — API keys and credentials are encrypted at rest, never exposed to agents directly

AgentOS runs as a process on your local machine. It exposes a CLI (`agentctl`) for management, and allows multiple LLMs to be connected and routed simultaneously.

---

## Core Principles

| Principle                       | Description                                                                     |
| ------------------------------- | ------------------------------------------------------------------------------- |
| **Security is non-negotiable**  | Capability-based isolation, encrypted secrets vault, no feature trades security |
| **Minimal by design**           | Every component exists for a reason; nothing more                               |
| **LLM-native, not LLM-wrapped** | Designed from first principles for agents                                       |
| **Multi-LLM by default**        | Connect OpenAI, Anthropic, Ollama, Gemini simultaneously                        |
| **Agents are social**           | Every agent knows what other agents exist and can collaborate                   |
| **Community extensible**        | Open tool ecosystem so anyone can build and install tools                       |

---

## How is AgentOS Different?

| Characteristic  | Traditional OS            | AgentOS                          |
| --------------- | ------------------------- | -------------------------------- |
| Primary user    | Human                     | LLM / AI Agent                   |
| Interface       | Terminal / GUI            | Semantic Intent + CLI            |
| IPC             | Pipes, sockets, signals   | Intent Channels (typed, async)   |
| Syscall         | Integer-keyed kernel call | Semantic Intent declaration      |
| Scheduler       | Process scheduler         | Inference task scheduler         |
| Memory          | RAM pages                 | Context windows + semantic store |
| Security        | User permissions + ACLs   | Capability tokens + sandboxing   |
| Credentials     | Env vars / config files   | Encrypted secrets vault          |
| Package manager | apt / pacman              | Tool Registry                    |

The key philosophical shift: **an LLM does not "execute" tools the way a human runs a program**. An LLM _declares intent_, and the kernel _decides_ whether to honor it, which tool handles it, and how the result flows back into context.

---

## Linux ↔ AgentOS Analogy

If you are familiar with Linux, the following mapping will help you understand every AgentOS concept:

```
Linux                          AgentOS
─────────────────────────────────────────────────────────────────
Kernel                    →    Inference Kernel
Process                   →    Agent Task
System Call               →    Semantic Call (Intent)
Program / ELF Binary      →    Agent Tool (manifest + binary)
Shell (bash/zsh)          →    Intent Shell (agentctl CLI)
IPC (pipes/sockets)       →    Intent Channels + Agent Message Bus
Filesystem                →    Semantic Store
User / Group Permissions  →    Permission Matrix (rwx per resource)
Password / SSH Key        →    Secrets Vault (encrypted, kernel-managed)
Package Manager (apt)     →    Tool Registry
init / systemd            →    Task Supervisor (agentd)
cron                      →    Agent Scheduler
/proc virtual FS          →    Task Inspector API
```

---

## Current Status

AgentOS has completed **Phase 1 (V1 MVP)** and most of **Phase 2 (V2)**:

### V1 — Foundation (Complete ✅)

- Cargo workspace with 10 crates
- Core types, IDs, structured error handling
- Append-only audit log (SQLite)
- Encrypted secrets vault (AES-256-GCM + Argon2id)
- Capability engine (HMAC-SHA256 tokens + permission matrix)
- Intent bus (Unix domain sockets, length-prefixed JSON)
- Inference kernel (task scheduler, context manager)
- Ollama LLM adapter
- 5 core tools (file-reader, file-writer, memory-search, memory-write, data-parser)
- CLI (`agentctl`) with all command groups

### V2 — Production Features (Complete ✅)

- seccomp-BPF sandboxing for tool execution
- Multi-LLM adapters (OpenAI, Anthropic, Gemini, Custom)
- Task routing engine (capability-first, cost-first, latency-first, round-robin)
- Extended tools (shell-exec, agent-message, task-delegate)
- Agent-to-agent communication via Message Bus
- Role-Based Access Control (RBAC) with persistent roles
- Background tasks and scheduled jobs (`agentd` supervisor)

---

## Next Steps

After reading this introduction, continue with:

1. **[Getting Started](02-getting-started.md)** — Build and run AgentOS
2. **[Architecture](03-architecture.md)** — Deep dive into how the system works
3. **[CLI Reference](04-cli-reference.md)** — Complete command reference
4. **[Tools Guide](05-tools-guide.md)** — Understanding and using Agent Tools
5. **[Security Model](06-security.md)** — Secrets, permissions, sandboxing
6. **[Configuration](07-configuration.md)** — Configuring the kernel and subsystems
