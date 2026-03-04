# AgentOS V2 — Phase 2 Plans

> Phase 2 builds on the solid V1 foundation to deliver the features that make AgentOS a **production-ready, multi-LLM, multi-agent operating environment**.

---

## V1 Recap (What We Have)

| Component                                                            | Status |
| -------------------------------------------------------------------- | ------ |
| Cargo workspace (9 crates)                                           | ✅     |
| Core types, IDs, error handling                                      | ✅     |
| Append-only audit log (SQLite)                                       | ✅     |
| Encrypted secrets vault (AES-256-GCM + Argon2id)                     | ✅     |
| Capability engine (HMAC-SHA256 tokens + permission matrix)           | ✅     |
| Intent bus (Unix domain sockets, length-prefixed JSON)               | ✅     |
| Inference kernel (task scheduler, context manager, command handlers) | ✅     |
| Ollama LLM adapter                                                   | ✅     |
| 5 core tools (file-reader/writer, memory-search/write, data-parser)  | ✅     |
| CLI (`agentctl`) with all command groups                             | ✅     |
| Unit + integration tests                                             | ✅     |

---

## V2 Scope — What We're Building

Phase 2 is organized into **6 build steps**, each with a dedicated plan file. The steps are ordered by dependency — each step builds on the previous.

### Step Overview

| #   | Plan                                                      | Description                                                                          | New Crates / Major Changes                    |
| --- | --------------------------------------------------------- | ------------------------------------------------------------------------------------ | --------------------------------------------- |
| 01  | [seccomp Sandboxing](01-seccomp-sandboxing.md)            | Process-isolated tool execution with seccomp-BPF syscall filtering                   | `agentos-sandbox`                             |
| 02  | [Multi-LLM Adapters](02-multi-llm-adapters.md)            | OpenAI, Anthropic, Gemini adapters + task routing engine                             | `agentos-llm` extensions                      |
| 03  | [Extended Tools](03-extended-tools.md)                    | 5 new tools: `http-client`, `sys-monitor`, `log-reader`, `shell-exec`, `code-runner` | `agentos-tools` extensions                    |
| 04  | [Agent-to-Agent Communication](04-agent-communication.md) | Agent message bus, task delegation, `agent-message` + `task-delegate` tools          | `agentos-agent-bus`, kernel changes           |
| 05  | [Advanced Permissions & Memory](05-permissions-memory.md) | Permission profiles, time-limited perms, episodic memory, vector semantic memory     | `agentos-memory`, kernel + capability changes |
| 06  | [Background Tasks & agentd](06-agentd-background.md)      | `agentd` supervisor, cron scheduler, detached background tasks                       | `agentos-agentd`, CLI extensions              |

---

## Features Explicitly Deferred to Phase 3+

The following are described in the spec but **out of scope** for Phase 2:

| Feature                                                   | Target Phase |
| --------------------------------------------------------- | ------------ |
| Web UI (Axum + HTMX dashboard)                            | Phase 3      |
| Hardware Abstraction Layer (HAL) — sensors, GPIO, drivers | Phase 3      |
| GPU Resource Manager (CUDA/Metal/Vulkan)                  | Phase 3      |
| WASM tool support (Wasmtime runtime)                      | Phase 3      |
| Python SDK (`agentos` on PyPI)                            | Phase 3      |
| Node.js SDK (`@agentos/sdk` on npm)                       | Phase 3      |
| Rust tool SDK (`agentos-sdk` crate with proc macros)      | Phase 3      |
| Remote Tool Registry (hosted)                             | Phase 3      |
| Multi-agent pipelines (YAML pipeline engine)              | Phase 3      |
| Docker production deployment (multi-stage builds)         | Phase 3      |
| Agent identity persistence across restarts                | Phase 3+     |
| Prompt injection red-teaming                              | Phase 3+     |
| Formal verification of capability model                   | Phase 6+     |

---

## Build Order & Dependencies

```
Phase 2 Build Graph:

  01-seccomp ──────┐
                   │
  02-multi-llm ────┤
                   │
                   ├──→ 03-extended-tools  (needs seccomp for safe execution)
                   │
                   ├──→ 04-agent-communication  (needs multi-LLM for multi-agent)
                   │
                   ├──→ 05-permissions-memory  (needs agent-comm for scoped memory)
                   │
                   └──→ 06-agentd  (needs all above subsystems)
```

Steps 01 and 02 can be built in **parallel**. Steps 03-06 are sequential.

---

## New Dependencies (Phase 2)

```toml
# Additions to workspace Cargo.toml [workspace.dependencies]

# Seccomp (Linux syscall filtering)
seccompiler = "0.4"           # AWS Firecracker's seccomp library

# Multi-LLM adapters
# reqwest already in workspace (for HTTP API calls)

# System monitoring
sysinfo = "0.32"              # CPU, RAM, disk, process info

# Vector embeddings (semantic memory upgrade)
qdrant-client = "1"           # Qdrant vector DB client (or use SQLite + custom similarity)
# OR for embedded: usearch = "2"  # Embedded vector search

# Cron scheduling
cron = "0.12"                 # Cron expression parsing
```

---

## Verification Strategy

Each plan step includes:

1. **Unit tests** for new modules (in-crate `#[test]` modules)
2. **Integration tests** (in `crates/agentos-cli/tests/` or workspace-level `tests/`)
3. **Manual E2E tests** with step-by-step CLI commands

### Running All Tests

```bash
# Unit tests (all crates)
cargo test --workspace

# Integration tests that need Ollama
cargo test --workspace -- --ignored

# Clippy + format check
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Estimated Timeline

| Step                      | Effort       | Calendar     |
| ------------------------- | ------------ | ------------ |
| 01 — seccomp              | ~3 days      | Week 1       |
| 02 — Multi-LLM            | ~4 days      | Week 1-2     |
| 03 — Extended Tools       | ~3 days      | Week 2       |
| 04 — Agent Communication  | ~5 days      | Week 3       |
| 05 — Permissions & Memory | ~4 days      | Week 4       |
| 06 — agentd               | ~4 days      | Week 4-5     |
| **Total**                 | **~23 days** | **~5 weeks** |
