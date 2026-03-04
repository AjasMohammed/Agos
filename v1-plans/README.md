# AgentOS v1 (Phase 1) — Implementation Plans

> Detailed, structured plans for building the AgentOS MVP. Each plan file is self-contained and can be handed directly to an AI coding tool.

## What Phase 1 Delivers

A working single-LLM AgentOS that can:

1. Connect to a local Ollama instance
2. Accept natural language tasks via CLI
3. Route intents through the kernel to sandboxed tools
4. Read/write files, search/write memory, parse data
5. Store API keys in an encrypted vault
6. Enforce capability-based permissions on every operation
7. Log every action to an immutable audit log

## Build Order (Strict — Each Step Depends on Previous)

```
Step 1: Project Setup          → Cargo workspace, dependencies, folder structure
Step 2: Core Types             → All shared types used across every crate
Step 3: Audit Log              → Append-only log (needed by kernel + tools)
Step 4: Secrets Vault          → Encrypted credential storage
Step 5: Capability Engine      → HMAC-signed tokens, permission matrix
Step 6: Intent Bus             → Unix domain socket IPC
Step 7: Inference Kernel       → Task scheduler, context manager
Step 8: Ollama Adapter         → First LLM backend
Step 9: Core Tools             → 5 built-in tools
Step 10: CLI (agentctl)        → User-facing command interface
Step 11: Integration Testing   → End-to-end validation
```

## Plan Files

| File                                                     | Description                                      | Est. LoC |
| -------------------------------------------------------- | ------------------------------------------------ | -------- |
| [01-project-setup.md](./01-project-setup.md)             | Cargo workspace, crate structure, dependencies   | ~200     |
| [02-core-types.md](./02-core-types.md)                   | Shared types: IDs, IntentMessage, tokens, errors | ~800     |
| [03-audit-log.md](./03-audit-log.md)                     | Append-only SQLite audit log                     | ~400     |
| [04-secrets-vault.md](./04-secrets-vault.md)             | AES-256-GCM encrypted vault with SQLCipher       | ~600     |
| [05-capability-engine.md](./05-capability-engine.md)     | Capability tokens + permission matrix            | ~700     |
| [06-intent-bus.md](./06-intent-bus.md)                   | Unix domain socket IPC layer                     | ~800     |
| [07-inference-kernel.md](./07-inference-kernel.md)       | Task scheduler, context manager, kernel loop     | ~1500    |
| [08-ollama-adapter.md](./08-ollama-adapter.md)           | Ollama REST API adapter                          | ~500     |
| [09-core-tools.md](./09-core-tools.md)                   | 5 built-in tools with manifests                  | ~1200    |
| [10-cli.md](./10-cli.md)                                 | `agentctl` CLI with clap                         | ~800     |
| [11-integration-testing.md](./11-integration-testing.md) | End-to-end test plan                             | ~400     |

**Total estimated LoC: ~8,000 — 10,000** (Rust)

## Architecture Diagram (Phase 1 Scope Only)

```
┌──────────────────────────────────────────────────┐
│                  AgentOS v1                       │
│                                                  │
│  ┌──────────────┐                                │
│  │  agentctl    │  (CLI — clap-based)            │
│  │  CLI Client  │                                │
│  └──────┬───────┘                                │
│         │ Unix Domain Socket                     │
│  ┌──────▼────────────────────────────────────┐   │
│  │           Inference Kernel                 │   │
│  │                                           │   │
│  │  ┌────────────┐  ┌──────────────────────┐ │   │
│  │  │Task Sched  │  │ Capability Engine    │ │   │
│  │  ├────────────┤  │ (tokens + perm matrix)│ │   │
│  │  │Context Mgr │  └──────────────────────┘ │   │
│  │  ├────────────┤  ┌──────────────────────┐ │   │
│  │  │Audit Log   │  │ Secrets Vault        │ │   │
│  │  └────────────┘  │ (AES-256 + SQLCipher)│ │   │
│  │                  └──────────────────────┘ │   │
│  └──────┬──────────────────┬─────────────────┘   │
│         │                  │                      │
│  ┌──────▼──────┐    ┌──────▼──────┐              │
│  │ Ollama      │    │ Intent Bus  │              │
│  │ Adapter     │    │ (UDS IPC)   │              │
│  └─────────────┘    └──────┬──────┘              │
│                            │                      │
│                     ┌──────▼──────────────────┐   │
│                     │    Core Tools            │   │
│                     │                         │   │
│                     │  file-reader            │   │
│                     │  file-writer            │   │
│                     │  memory-search          │   │
│                     │  memory-write           │   │
│                     │  data-parser            │   │
│                     └─────────────────────────┘   │
└──────────────────────────────────────────────────┘
```

## Out of Scope for Phase 1

- ❌ Multi-LLM routing (only Ollama)
- ❌ Web UI
- ❌ Hardware Abstraction Layer / GPU
- ❌ Agent-to-agent communication
- ❌ Multi-agent pipelines
- ❌ Background tasks / cron (`agentd`)
- ❌ Python / Node.js SDKs
- ❌ WASM tool support (native Rust tools only)
- ❌ Tool Registry (remote)
- ❌ seccomp sandboxing (deferred to Phase 2 — use process isolation only)

## Conventions

- **Rust edition**: 2021
- **Async runtime**: `tokio` (multi-threaded)
- **Serialization**: `serde` + `serde_json` for JSON, `toml` for config
- **Error handling**: `thiserror` for library errors, `anyhow` for CLI
- **Logging**: `tracing` + `tracing-subscriber`
- **Testing**: `#[tokio::test]` for async tests, standard `#[test]` otherwise
