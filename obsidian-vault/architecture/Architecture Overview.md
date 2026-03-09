---
title: Architecture Overview
tags: [architecture, design]
---

# Architecture Overview

AgentOS is designed as a microkernel-style OS where the Inference Kernel orchestrates all agent operations, tool executions, and inter-agent communication.

## High-Level Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                        agentctl (CLI)                           │
│              clap-based command parser + formatter              │
└──────────────────────────┬──────────────────────────────────────┘
                           │ Unix Domain Socket
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Intent Bus (agentos-bus)                      │
│          BusServer ◄──► BusClient (bidirectional)               │
└──────────────────────────┬──────────────────────────────────────┘
                           │ BusMessage (Command/Intent/Response)
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Inference Kernel (agentos-kernel)              │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │ Task         │  │ Task Router  │  │ Context Manager      │  │
│  │ Scheduler    │  │ (strategies) │  │ (per-task windows)   │  │
│  └──────┬───────┘  └──────┬───────┘  └──────────┬───────────┘  │
│         │                 │                      │              │
│  ┌──────┴───────┐  ┌──────┴───────┐  ┌──────────┴───────────┐  │
│  │ Agent        │  │ Capability   │  │ Agent Message Bus    │  │
│  │ Registry     │  │ Engine       │  │ (direct/group/bcast) │  │
│  └──────────────┘  └──────────────┘  └──────────────────────┘  │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │ Schedule     │  │ Background   │  │ Pipeline Engine      │  │
│  │ Manager      │  │ Pool         │  │ (YAML workflows)     │  │
│  └──────────────┘  └──────────────┘  └──────────────────────┘  │
└──────┬──────────────────┬──────────────────┬────────────────────┘
       │                  │                  │
       ▼                  ▼                  ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────────────────┐
│ LLM Adapters │  │ Tool Runner  │  │ Support Systems          │
│              │  │              │  │                           │
│ - Ollama     │  │ - file-reader│  │ - Audit Log (SQLite)     │
│ - OpenAI     │  │ - file-writer│  │ - Vault (AES-256-GCM)    │
│ - Anthropic  │  │ - memory-*   │  │ - Sandbox (seccomp-BPF)  │
│ - Gemini     │  │ - shell-exec │  │ - WASM Runtime           │
│ - Custom     │  │ - http-client│  │ - HAL (drivers)          │
│              │  │ - sys-monitor│  │ - Memory (semantic+      │
│              │  │ - agent-msg  │  │   episodic)              │
│              │  │ - task-deleg.│  │                           │
└──────────────┘  └──────────────┘  └──────────────────────────┘
```

## Design Principles

1. **LLMs are first-class citizens** - The kernel treats agents (LLMs) as the primary processing entities
2. **Intent-driven** - All operations expressed as typed intents, not raw function calls
3. **Capability-secured** - Every operation requires an unforgeable, time-limited token
4. **Async-first** - Tokio throughout, no blocking I/O anywhere
5. **Modular** - Each subsystem in its own crate with minimal coupling
6. **Zero-trust** - Agents start with no permissions; everything is explicitly granted

## Crate Architecture

See [[Crate Dependency Map]] for the full dependency graph.

### Core Layer
- **[[Type System|agentos-types]]** - Shared types, IDs, errors (no dependencies)

### Infrastructure Layer
- **agentos-audit** - Append-only audit log
- **agentos-vault** - Encrypted secrets storage
- **agentos-capability** - Token issuing and validation
- **agentos-bus** - IPC transport layer

### Service Layer
- **agentos-llm** - LLM provider adapters
- **agentos-tools** - Tool implementations and runner
- **agentos-sandbox** - Seccomp-BPF process isolation
- **agentos-wasm** - Wasmtime tool executor
- **agentos-memory** - Semantic + episodic memory
- **agentos-hal** - Hardware abstraction drivers
- **agentos-pipeline** - Multi-step workflow engine

### Orchestration Layer
- **agentos-kernel** - Central orchestrator connecting all subsystems

### Interface Layer
- **agentos-cli** - User-facing CLI (`agentctl`)
- **agentos-web** - Web dashboard (planned)

## Key Subsystem Interactions

### Task Execution
The kernel's [[Task Execution Flow|task loop]] coordinates between the scheduler, LLM adapters, tool runner, and context manager to execute agent tasks.

### Security Enforcement
Every tool call passes through the [[Capability and Permissions|capability engine]] which validates HMAC-signed tokens before execution is allowed.

### Communication
Agents communicate via the [[Agent Communication Flow|Agent Message Bus]] for direct messaging, group channels, and broadcast.
