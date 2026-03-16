---
title: Introduction and Philosophy
tags:
  - docs
  - handbook
date: 2026-03-16
status: complete
---

# Introduction and Philosophy

> AgentOS is a minimalist, LLM-native operating system written in Rust, designed for AI agents rather than humans.

---

## What is AgentOS?

AgentOS is a **purpose-built operating environment** where **LLMs are the primary users**, not humans. Unlike traditional AI agent frameworks that wrap LLMs around existing operating systems, AgentOS is built from scratch around a radical idea: treat LLMs as first-class compute units with their own kernel, scheduling, memory management, and security model. It runs as a process on your local machine, exposes a CLI (`agentctl`) for management, and allows multiple LLMs to be connected and routed simultaneously.

---

## Core Principles

| Principle | Description |
|-----------|-------------|
| **Security is non-negotiable** | Capability-based isolation, encrypted secrets vault, seccomp sandboxing. No feature trades security. |
| **Minimal by design** | Every component exists for a reason; nothing more. |
| **LLM-native, not LLM-wrapped** | Designed from first principles for agents, not retrofitted onto human-centric abstractions. |
| **Multi-LLM by default** | Connect OpenAI, Anthropic, Ollama, Gemini simultaneously with intelligent routing. |
| **Agents are social** | Every agent knows what other agents exist and can collaborate via a message bus. |
| **Community extensible** | Open tool ecosystem so anyone can build, sign, and install tools. |

---

## Linux ↔ AgentOS Analogy

If you are familiar with Linux, the following mapping will help you understand every AgentOS concept:

| Linux | AgentOS | Notes |
|-------|---------|-------|
| Kernel | Inference Kernel | Central orchestrator — scheduler, router, context, agent registry |
| Process | Agent Task | A unit of work dispatched to an LLM agent |
| System Call | Semantic Intent | Structured declaration instead of integer-keyed kernel call |
| Program / ELF Binary | Agent Tool | Manifest + executable (native or WASM), versioned and sandboxed |
| Shell (bash/zsh) | Intent Shell (`agentctl`) | CLI entry point for humans to manage the system |
| IPC (pipes/sockets) | Intent Channels + Agent Message Bus | Typed, async communication between agents |
| Filesystem | Semantic Store | Multi-tier memory with embeddings (working, episodic, semantic) |
| User / Group Permissions | Permission Matrix | rwx-style permissions per resource, capability tokens |
| Password / SSH Key | Secrets Vault | AES-256-GCM encrypted, Argon2id key derivation, kernel-managed |
| Package Manager (apt) | Tool Registry | Install, verify, and manage agent tools with trust tiers |
| init / systemd | Task Supervisor (`agentd`) | Background task management and scheduled jobs |
| cron | Agent Scheduler | Cron-expression scheduled agent tasks |
| /proc virtual FS | Task Inspector API | Introspect running tasks, agent state, and system health |

---

## How AgentOS Differs from Traditional AI Frameworks

The key philosophical shift: **an LLM does not "execute" tools the way a human runs a program**. An LLM _declares intent_, and the kernel _decides_ whether to honor it, which tool handles it, and how the result flows back into context.

| Aspect | LangChain / CrewAI / LangGraph | AgentOS |
|--------|-------------------------------|---------|
| **Architecture** | Library wrapping LLM API calls | Full operating environment with kernel, scheduler, and security |
| **Security model** | Trust the developer | Zero-trust: capability tokens, HMAC-signed permissions, seccomp sandboxing |
| **Secrets handling** | Environment variables or config files | Encrypted vault (AES-256-GCM + Argon2id), never exposed to agents |
| **Multi-LLM** | Manual provider switching | Built-in routing engine with 4 strategies (capability, cost, latency, round-robin) |
| **Agent collaboration** | Framework-specific orchestration | Native message bus with direct, delegation, and broadcast modes |
| **Tool execution** | Direct function calls | Sandboxed execution with capability validation, audit logging, and optional WASM isolation |
| **Memory** | Vector DB bolted on | Native 3-tier memory architecture (working, episodic, semantic) with embeddings |
| **Audit trail** | Optional logging | Mandatory append-only SQLite audit log (83+ event types) |
| **Cost control** | Manual tracking | Per-agent budgets with automatic model downgrade and hard limits |
| **Extensibility** | Python packages | Signed tool manifests with trust tiers (Core / Verified / Community / Blocked) |

---

## Crate Overview

AgentOS is organized as a Rust workspace with 17 crates. Each crate has a single responsibility; the dependency graph flows downward with no circular dependencies.

| Crate | Description |
|-------|-------------|
| `agentos-types` | Shared type definitions — IDs, IntentMessage, AgentTask, error types |
| `agentos-kernel` | Central orchestrator — scheduler, router, context manager, agent registry |
| `agentos-cli` | CLI binary `agentctl` (clap-based, 17+ command groups) |
| `agentos-bus` | Unix domain socket IPC between CLI and kernel |
| `agentos-llm` | LLM adapter trait + Ollama, OpenAI, Anthropic, Gemini, Mock implementations |
| `agentos-tools` | Built-in tools (file I/O, memory, shell, data parser, signing, etc.) |
| `agentos-capability` | HMAC-SHA256 signed capability tokens and permission system |
| `agentos-vault` | AES-256-GCM encrypted secrets store with Argon2id key derivation |
| `agentos-audit` | Append-only SQLite audit log (83+ event types) |
| `agentos-memory` | Multi-tier memory (episodic + semantic + procedural) with embeddings |
| `agentos-pipeline` | Multi-step workflow orchestration engine |
| `agentos-sandbox` | Seccomp-BPF syscall filtering (Linux-only) |
| `agentos-wasm` | WASM tool execution via Wasmtime |
| `agentos-hal` | Hardware Abstraction Layer (system, process, network, GPU, storage, sensors) |
| `agentos-sdk` | Ergonomic macros and re-exports for tool development |
| `agentos-sdk-macros` | Proc-macro crate for `#[tool]` attribute |
| `agentos-web` | Web UI server (Axum + HTMX, under development) |

---

## Current Status

AgentOS is on **V3** — the third major development phase.

### V1 — Foundation (Complete)

- Cargo workspace with core crates
- Shared types, UUID-based IDs, structured error handling (`thiserror`)
- Append-only audit log (SQLite)
- Encrypted secrets vault (AES-256-GCM + Argon2id)
- Capability engine (HMAC-SHA256 tokens + permission matrix)
- Intent bus (Unix domain sockets, length-prefixed JSON)
- Inference kernel (task scheduler, context manager)
- Ollama LLM adapter
- 5 core tools (file-reader, file-writer, memory-search, memory-write, data-parser)
- CLI (`agentctl`) with all command groups

### V2 — Production Features (Complete)

- Seccomp-BPF sandboxing for tool execution
- Multi-LLM adapters (OpenAI, Anthropic, Gemini, Custom)
- Task routing engine (4 strategies + pattern-based rules)
- Extended tools (shell-exec, agent-message, task-delegate)
- Agent-to-agent communication via Message Bus
- Role-Based Access Control (RBAC) with persistent roles
- Background tasks and scheduled jobs (`agentd` supervisor)

### V3 — Hardening and Advanced Systems (In Progress)

- Ed25519 tool signing and trust tier enforcement (Core / Verified / Community / Blocked)
- Cost tracking with per-agent budgets and automatic model downgrade
- Escalation system with expiry, soft-approvals, and webhook notifications
- Resource arbitration for shared system resources
- Injection scanning and risk classification
- Intent validation with JSON Schema
- Event bus with subscription filtering and throttling
- Snapshot management with expiration sweeps
- Hardware Abstraction Layer (6 driver types)
- Pipeline orchestration engine
- WASM tool execution runtime

---

## How to Read This Handbook

This handbook is organized so that each chapter builds on the previous ones. We recommend reading the foundation chapters in order:

1. **[[01-Introduction and Philosophy]]** — You are here. Understand what AgentOS is and why it exists.
2. **[[02-Installation and First Run]]** — Build from source, configure, and run your first agent task.
3. **[[03-Architecture Overview]]** — Deep dive into the kernel, routing, memory, events, and security.

After the foundation, chapters can be read in any order based on your needs:

- **CLI Reference** — Complete `agentctl` command reference
- **Tools Guide** — Understanding, using, and building agent tools
- **Security Model** — Secrets vault, capability tokens, sandboxing, trust tiers
- **Configuration** — All kernel and subsystem configuration options
- **Memory System** — Working, episodic, and semantic memory tiers
- **Cost and Budgets** — Per-agent cost tracking, budget enforcement, model downgrade
- **Event System** — Subscriptions, filtering, throttling, and triggered tasks
