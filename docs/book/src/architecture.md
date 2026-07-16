# Architecture

AgentOS is a Rust workspace whose crates form a strictly downward dependency graph — no
cycles. The **Inference Kernel** is the central orchestrator; the `agentos` CLI is a thin
client that talks to it over a Unix domain socket.

## System overview

```
agentos CLI ──(Unix domain socket / Intent Bus)──> Inference Kernel
                                                      │
   ┌──────────────────────────────────────────────────┴───────────────────────┐
   │  Task Scheduler · Context Manager · Agent Registry · Task Router          │
   │  Capability Engine · Secrets Vault · Audit Log · Schedule Manager         │
   └──────────────┬──────────────────────────────────┬────────────────────────┘
        LLM Adapters                          Tool Registry + Sandbox
  Ollama · OpenAI · Anthropic ·       file/memory/data/shell tools, WASM tools,
  Gemini · Custom                     sandboxed via seccomp-BPF / bwrap / Wasmtime
```

## Intent flow

A tool call moves through the kernel as a sequence of mediated steps:

```
LLM emits intent → IntentMessage on the bus
  → Capability-token validation (against the required PermissionSet)
  → Intent schema validation
  → Tool execution (sandboxed where applicable)
  → Result injected into the agent's ContextWindow
  → AuditLog entry written
```

The LLM never "executes" a tool directly. It *declares intent*; the kernel *decides* whether
to honor it, which tool handles it, and how the result flows back into context.

## Key crates

| Crate | Responsibility |
|-------|----------------|
| `agentos-types` | Shared types: IDs, `IntentMessage`, `AgentTask`, errors. Re-exported at crate root. |
| `agentos-kernel` | Scheduler, router, context manager, agent registry, command handlers. |
| `agentos-cli` | The `agentos` binary (clap). |
| `agentos-bus` | Unix domain socket IPC (length-prefixed JSON). |
| `agentos-llm` | `LLMCore` adapter trait + Ollama/OpenAI/Anthropic/Gemini/Custom/Mock. |
| `agentos-tools` | Built-in tool implementations. |
| `agentos-capability` | HMAC-SHA256 capability tokens + permission engine. |
| `agentos-vault` | AES-256-GCM encrypted secrets store (Argon2id). |
| `agentos-audit` | Append-only, Merkle-chained SQLite audit log. |
| `agentos-memory` | Multi-tier memory with FTS5 + vector retrieval. |
| `agentos-sandbox` | seccomp-BPF syscall filtering (Linux-only). |
| `agentos-wasm` | WASM tool execution via Wasmtime. |
| `agentos-pipeline` | Multi-step workflow orchestration. |
| `agentos-api` | REST endpoints + OpenAI-compatible `/v1/chat/completions` SSE. |
| `agentos-web` | Web UI (Axum + HTMX). |
| `agentos-channels` | Channel adapters (Discord, Slack, Telegram, Matrix, …). |
| `agentos-skills` | Skill manifests and registry. |
| `agentos-mcp` | Model Context Protocol client/server + A2A. |

## Command handling

Commands are dispatched from the kernel run loop to per-domain handlers in
`crates/agentos-kernel/src/commands/`. The flow is: CLI command → `BusMessage` →
`KernelCommand` → run-loop dispatch → handler → audit entry.

## Concurrency model

Shared state uses `Arc<RwLock<T>>`; polymorphic adapters are `Arc<dyn Trait + Send + Sync>`.
Shutdown is signalled with a `CancellationToken` that is propagated, not dropped. Blocking
I/O runs on `tokio::task::spawn_blocking` so the async runtime is never blocked.
