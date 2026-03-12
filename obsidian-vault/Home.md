---
title: AgentOS Knowledge Base
tags:
  - moc
---

# AgentOS Knowledge Base

> A minimalist, LLM-native operating system built in Rust where **LLMs are the primary users**, not humans.

---

## Quick Navigation

### Getting Started
- [[Getting Started]] - Build, configure, and run AgentOS
- [[Configuration Guide]] - TOML configuration reference
- [[CLI Reference]] - Complete `agentctl` command reference

### Architecture
- [[Architecture Overview]] - High-level system design
- [[Kernel Deep Dive]] - Boot sequence, subsystems, and internals
- [[Crate Dependency Map]] - How the 16 crates relate
- [[Type System]] - Core data structures and ID types

### Core Systems
- [[Tool System]] - Built-in tools, manifests, WASM, and the AgentTool trait
- [[LLM Integration]] - Multi-provider adapter layer
- [[Message Bus]] - Unix socket IPC and intent messages
- [[Capability and Permissions]] - Tokens, RBAC, and permission model
- [[Memory System]] - Semantic vectors + episodic memory
- [[Pipeline System]] - YAML-defined multi-agent workflows
- [[HAL System]] - Hardware abstraction layer

### Security & Audit
- [[Security Model]] - 7-layer security architecture
- [[Vault and Secrets]] - AES-256-GCM encrypted secrets store
- [[Audit System]] - Append-only immutable audit log

### Flows & Lifecycle
- [[Agent Lifecycle]] - Connect, execute, communicate, disconnect
- [[Task Execution Flow]] - From prompt to completion
- [[Intent Processing Flow]] - Tool call validation and execution
- [[Agent Communication Flow]] - Inter-agent messaging

### Roadmap & Next Steps
- [[V3 Roadmap]] - Planned features and build steps
- [[Feedback Implementation Plan]] - Phase 0–7 implementation plan
- [[next-steps/Index|Next Steps Dashboard]] - Live implementation status + what's left to build
  - [[next-steps/01-Critical Build Fix|01 · Critical Build Fix]] — Fix broken AuditEntry initializers (blocker)
  - [[next-steps/02-Ed25519 Tool Signing|02 · Ed25519 Tool Signing]] — Spec #1: capability-signed tool registry
  - [[next-steps/03-Snapshot Rollback|03 · Snapshot/Rollback]] — Spec #5: checkpoint and rollback system
  - [[next-steps/04-Kernel Modularization|04 · Kernel Modularization]] — Phase 0.1: split kernel.rs
  - [[next-steps/05-Episodic Memory Completion|05 · Episodic Memory]] — Phase 5.1: auto-write on task completion
  - [[next-steps/06-Command Bus Wiring|06 · Command Bus Wiring]] — Verify router dispatch coverage
  - [[next-steps/07-Hardware Abstraction Layer|07 · Hardware Abstraction Layer]] — Spec #9: per-agent HAL

---

## Core Philosophy

| Traditional OS | AgentOS |
|---|---|
| Kernel manages processes | Inference Kernel manages LLM tasks |
| Syscalls (open, read, write) | Semantic Intents (Read, Write, Execute, Query) |
| Programs (ELF binaries) | Tools (Rust inline + WASM modules) |
| IPC (pipes, sockets) | Intent Channels + Agent Message Bus |
| Filesystem | Semantic Memory Store |
| Unix permissions (rwx) | Capability Tokens + Permission Matrix |
| /etc/shadow | Encrypted Vault (AES-256-GCM) |
| Package manager | Tool Registry |
| init/systemd | Task Supervisor |
| cron | Agent Scheduler |

## Tech Stack

| Component | Technology |
|---|---|
| Language | Rust 2021 Edition |
| Async Runtime | Tokio (multi-threaded) |
| Database | SQLite (rusqlite, bundled) |
| Encryption | AES-256-GCM + Argon2id + HMAC-SHA256 |
| IPC | Unix domain sockets |
| CLI | clap (derive macros) |
| WASM Runtime | Wasmtime 38 |
| Sandbox | seccomp-BPF + bwrap |
| Embeddings | ONNX Runtime + MiniLM-L6-v2 |
| Web (planned) | Axum + HTMX |
