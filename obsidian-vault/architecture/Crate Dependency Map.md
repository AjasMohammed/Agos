---
title: Crate Dependency Map
tags: [architecture, dependencies]
---

# Crate Dependency Map

AgentOS consists of 16 crates with a clean, top-down dependency graph and no circular dependencies.

## Dependency Graph

```
                        agentos-cli
                       /           \
                      /             \
              agentos-bus        agentos-kernel
              (BusClient)       (Kernel boot)
                                     |
                    ┌────────────────┼────────────────────┐
                    │                │                     │
              agentos-llm     agentos-tools         agentos-pipeline
              (LLM adapters)  (ToolRunner)          (PipelineEngine)
                    │                │                     │
                    │         ┌──────┼──────┐              │
                    │         │      │      │              │
                    │   agentos-  agentos-  agentos-       │
                    │   memory    hal       wasm           │
                    │                                      │
                    ├──────────────┬───────────────────────┘
                    │              │
              agentos-audit  agentos-vault  agentos-capability  agentos-sandbox
              (AuditLog)     (SecretsVault)  (CapEngine)        (Seccomp)
                    │              │              │                  │
                    └──────────────┴──────────────┴──────────────────┘
                                          │
                                    agentos-types
                                   (shared types)
```

## Crate Summary

| Crate | Purpose | Key Exports |
|---|---|---|
| `agentos-types` | Shared types and IDs | `TaskID`, `AgentID`, `AgentTask`, `IntentMessage`, `PermissionSet` |
| `agentos-audit` | Append-only audit log | `AuditLog`, `AuditEntry`, `AuditEventType` |
| `agentos-vault` | Encrypted secrets | `SecretsVault`, `SecretEntry` |
| `agentos-capability` | Token engine | `CapabilityEngine`, `CapabilityToken`, `PermissionEntry` |
| `agentos-bus` | IPC transport | `BusServer`, `BusClient`, `BusMessage` |
| `agentos-llm` | LLM adapters | `LLMCore` trait, `OllamaAdapter`, `OpenAIAdapter`, etc. |
| `agentos-tools` | Tool runner | `ToolRunner`, `AgentTool` trait, built-in tools |
| `agentos-sandbox` | Process isolation | `SandboxExecutor`, `SandboxConfig`, seccomp filters |
| `agentos-wasm` | WASM executor | `WasmToolExecutor`, Wasmtime integration |
| `agentos-memory` | Memory stores | `SemanticStore`, `EpisodicStore`, `Embedder` |
| `agentos-hal` | Hardware drivers | `HalDriver` trait, `SystemDriver`, `ProcessDriver` |
| `agentos-pipeline` | Workflow engine | `PipelineEngine`, `PipelineDefinition`, `PipelineStep` |
| `agentos-kernel` | Central orchestrator | `Kernel`, `TaskScheduler`, `TaskRouter`, `ContextManager` |
| `agentos-cli` | CLI interface | `agentctl` binary, command handlers |
| `agentos-web` | Web UI (planned) | Axum + HTMX dashboard |

## Dependency Rules

1. **No circular dependencies** - Dependencies flow strictly downward
2. **All crates depend on `agentos-types`** - Shared type definitions
3. **Only `agentos-kernel` depends on service-layer crates** - Clean separation
4. **`agentos-cli` depends on kernel + bus** - Boot kernel or connect as client
