# AgentOS V3 — Phase 3 Implementation Plans

> Phase 3 tackles the most impactful remaining features from the spec: completing the standard tool library, upgrading the memory architecture, enabling multi-agent pipelines, building the Web UI, and shipping Docker deployment.

---

## V2 Recap (What We Have After Phase 2)

| Component                                           | Status |
| --------------------------------------------------- | ------ |
| seccomp-BPF sandbox (`agentos-sandbox`)             | ✅     |
| OpenAI, Anthropic, Gemini adapters + task routing   | ✅     |
| `shell-exec` tool + extended tool set               | ✅     |
| Agent Message Bus — direct messaging + broadcast    | ✅     |
| Task delegation (`task-delegate` tool)              | ✅     |
| RBAC roles + persistent permission profiles         | ✅     |
| `agentd` supervisor + cron scheduling               | ✅     |
| **WASM tool support via Wasmtime** (`agentos-wasm`) | ✅     |
| Text-based semantic memory search                   | ✅     |
| Episodic memory (basic SQLite per-task)             | ✅     |

---

## V3 Scope — What We're Building

Phase 3 is organized into **7 build steps**, each with a dedicated plan file.

### Step Overview

| #   | Plan                                                 | Description                                                            | New Crates / Major Changes               |
| --- | ---------------------------------------------------- | ---------------------------------------------------------------------- | ---------------------------------------- |
| 01  | [http-client Tool](01-http-client-tool.md)           | Outbound HTTP tool with vault-backed auth headers                      | `agentos-tools` extension                |
| 02  | [HAL & System Tools](02-hal-system-tools.md)         | Hardware Abstraction Layer + sys-monitor, process-manager, log-reader  | `agentos-hal`                            |
| 03  | [Memory Architecture Upgrade](03-memory-upgrade.md)  | Real vector embeddings for semantic memory + episodic memory indexing  | `agentos-memory` (new), `fastembed-rs`   |
| 04  | [Multi-Agent Pipelines](04-multi-agent-pipelines.md) | YAML pipeline definitions, sequential agent chaining, pipeline CLI     | `agentos-pipeline` (new), kernel changes |
| 05  | [Web UI](05-web-ui.md)                               | Axum backend + HTMX frontend — dashboard, task inspector, agent view   | `agentos-web` (new)                      |
| 06  | [Docker Deployment](06-docker-deployment.md)         | Multi-stage Dockerfile, docker-compose, health checks, GPU passthrough | `Dockerfile`, `docker-compose.yml`       |
| 07  | [Rust Tool SDK](07-rust-tool-sdk.md)                 | `agentos-sdk` crate with proc-macro `#[tool(...)]` ergonomic API       | `agentos-sdk` (new)                      |

---

## Features Deferred to Phase 4+

| Feature                                       | Reason                             | Target |
| --------------------------------------------- | ---------------------------------- | ------ |
| Python SDK (`agentos` on PyPI)                | Requires stable SDK design from 07 | P4     |
| Node.js / TypeScript SDK (`@agentos/sdk`)     | Requires stable SDK design from 07 | P4     |
| Hosted Tool Registry with trust tiers         | Requires infra + code signing      | P4     |
| GPU Resource Manager (CUDA/Metal/Vulkan/ROCm) | Requires hardware to test          | P4     |
| HAL GPIO / sensor drivers (IoT)               | Hardware-specific, narrow audience | P4     |
| Agent identity persistence across restarts    | Needs crypto design (TPM/KDF)      | P4     |
| Prompt injection safety filter module         | Needs red-team testing phase       | P4     |
| AgentOS Cloud (multi-tenant hosted)           | Post MVP                           | P5+    |
| Federated agent networks                      | Post MVP                           | P5+    |
| Formal verification of capability model       | Research-level effort              | P6+    |

---

## Build Order & Dependencies

```
Phase 3 Build Graph:

  01-http-client ────┐
                     │
  02-hal-tools ──────┤
                     │
                     ├──→ 03-memory-upgrade (can start independently)
                     │
                     ├──→ 04-pipelines  (needs agent-comm from V2 + routing)
                     │
                     ├──→ 05-web-ui  (needs 01,02,03,04 for data to display)
                     │
                     ├──→ 06-docker  (needs the final binary to containerize)
                     │
                     └──→ 07-sdk  (can start after 01-02 stabilize)
```

Steps 01, 02, 03, and 07 can be built **in parallel**. Step 05 (Web UI) depends on all prior steps being functional. Step 06 (Docker) is the final packaging step.

---

## New Dependencies (Phase 3)

```toml
# Additions to workspace Cargo.toml [workspace.dependencies]

# HTTP client (already present via reqwest for LLM adapters)
# — no new dep needed for http-client tool

# System monitoring (process/CPU/RAM info)
sysinfo = "0.33"

# Vector embeddings (semantic memory)
fastembed = "4"           # Local ONNX-based embeddings (all-MiniLM-L6-v2)
# OR: candle-core + candle-transformers for Rust-native inference

# Web UI backend
axum = "0.8"
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "trace", "fs"] }
minijinja = "2"          # Templating for HTMX responses

# YAML pipeline definitions
serde_yaml = "0.9"       # Already popular; or use serde with toml for YAML
```

---

## Verification Strategy

Each plan step includes:

1. **Unit tests** — happy path, error cases, permission denial
2. **Integration tests** — end-to-end CLI invocations
3. **Manual E2E** — step-by-step validation commands listed per plan

### Running All Tests

```bash
# Unit tests (all crates)
cargo test --workspace

# Specific crate
cargo test -p agentos-memory

# Integration tests with live Ollama
cargo test --workspace -- --ignored

# Linting
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Estimated Timeline

| Step                       | Effort       | Parallel?    |
| -------------------------- | ------------ | ------------ |
| 01 — http-client tool      | ~1 day       | ✅ P         |
| 02 — HAL & system tools    | ~4 days      | ✅ P         |
| 03 — Memory architecture   | ~5 days      | ✅ P         |
| 04 — Multi-agent pipelines | ~5 days      | ➡ Seq        |
| 05 — Web UI                | ~7 days      | ➡ Seq        |
| 06 — Docker deployment     | ~2 days      | ➡ Seq        |
| 07 — Rust Tool SDK         | ~4 days      | ✅ P         |
| **Total**                  | **~28 days** | **~6 weeks** |
