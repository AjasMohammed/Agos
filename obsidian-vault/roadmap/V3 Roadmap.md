---
title: V3 Roadmap
tags: [roadmap, planning]
---

# V3 Roadmap

## Completed Phases

### V1 - Foundation
- Workspace and crate structure
- Core types (`agentos-types`)
- Audit log (`agentos-audit`)
- Encrypted vault (`agentos-vault`)
- Capability engine (`agentos-capability`)
- Intent bus (`agentos-bus`)
- Inference kernel (`agentos-kernel`)
- Ollama LLM adapter
- 5 core tools (file-reader, file-writer, memory-search, memory-write, data-parser)
- CLI (`agentctl`)

### V2 - Production Features
- seccomp-BPF sandboxing
- Multi-LLM adapters (OpenAI, Anthropic, Gemini, Custom)
- Task routing (4 strategies + regex rules)
- Extended tools (shell-exec, agent-message, task-delegate)
- Agent communication (direct, group, broadcast)
- RBAC (roles, profiles, time-limited permissions)
- Background tasks
- WASM tool support (Wasmtime 38)
- Text-based memory search
- Episodic memory

## V3 - Planned Features

### Build Dependency Graph

```
Independent (can build in parallel):
  ├── Step 01: HTTP Client Tool
  ├── Step 02: HAL + System Tools
  ├── Step 03: Memory Upgrade (Vector Embeddings)
  └── Step 07: Rust Tool SDK

Sequential (after above):
  Step 04: Multi-Agent Pipelines
  Step 05: Web UI (Axum + HTMX)
  Step 06: Docker Deployment
```

### Step 01: HTTP Client Tool
**Status:** Implemented

Outbound HTTP tool with vault-backed authentication:
- Methods: GET, POST, PUT, DELETE
- Secret injection via `secret_headers` (values pulled from vault)
- SSRF protection (blocks private IP ranges)
- Response size capping
- Redirect validation

### Step 02: HAL + System Tools
**Status:** Implemented

Hardware Abstraction Layer with pluggable drivers:
- `SystemDriver` - CPU, memory, uptime
- `ProcessDriver` - Process list/kill
- `NetworkDriver` - Interface stats
- `LogReaderDriver` - Structured log reading
- 5 new tools: sys-monitor, process-manager, log-reader, network-monitor, hardware-info

See [[HAL System]] for details.

### Step 03: Memory Upgrade
**Status:** Implemented

Vector embeddings for semantic memory:
- ONNX Runtime + MiniLM-L6-v2 (384-dim vectors)
- Hybrid search (cosine similarity + FTS5)
- Episodic memory with per-agent scoping

See [[Memory System]] for details.

### Step 04: Multi-Agent Pipelines
**Status:** Implemented

YAML-defined multi-step workflows:
- Topological dependency resolution
- Template variable substitution
- Agent tasks and tool calls as steps
- Retry logic and timeout management

See [[Pipeline System]] for details.

### Step 05: Web UI
**Status:** Planned

Dashboard built with Axum + HTMX:
- **Backend:** Axum 0.8 (shares Tokio runtime with kernel)
- **Frontend:** MiniJinja templates, HTMX 2.x, Alpine.js, Pico CSS
- **Real-time:** SSE for task log streaming
- **Pages:** Dashboard, Agent Manager, Task Inspector, Tool Manager, Secrets Manager, Pipeline Manager, Audit Log

### Step 06: Docker Deployment
**Status:** Planned

Multi-stage Docker build:
- ~47MB final image (Alpine-based)
- Non-root user
- Persistent volumes for data/vault
- docker-compose with Ollama service
- Health check endpoint

### Step 07: Rust Tool SDK
**Status:** Planned

Proc-macro SDK for ergonomic WASM tool development:
- `#[tool(...)]` attribute macro
- Auto-generates: main entry point, manifest TOML, error handling
- Eliminates boilerplate for custom tool authors

## New Dependencies (V3)

| Crate | Purpose |
|---|---|
| `sysinfo` | System/process information |
| `fastembed` / `ort` | ONNX embeddings |
| `axum` | Web framework |
| `tower` | HTTP middleware |
| `minijinja` | Template engine |
| `serde_yaml` | YAML parsing |
