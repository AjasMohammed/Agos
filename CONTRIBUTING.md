# Contributing to AgentOS

Thank you for your interest in contributing to AgentOS!

## Quick Start

```bash
git clone https://github.com/agentos/agentos
cd agentos
cargo build --workspace
cargo test --workspace
```

## Before You Submit

Every PR must pass:
```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

## Crate Structure

| Crate | Purpose |
|-------|---------|
| `agentos-types` | Shared types — change carefully, everything depends on this |
| `agentos-kernel` | Central orchestrator — scheduler, agent registry, command dispatch |
| `agentos-cli` | `agentos` binary — CLI commands via `agentos` |
| `agentos-bus` | Unix socket IPC between CLI and kernel |
| `agentos-llm` | LLM adapter trait + provider implementations |
| `agentos-tools` | Built-in tool implementations |
| `agentos-audit` | Append-only SQLite audit log |
| `agentos-memory` | Multi-tier memory (episodic, semantic, procedural) |
| `agentos-web` | Web UI (Axum + HTMX + Pico CSS) |
| `agentos-capability` | HMAC-SHA256 signed capability tokens and permission system |
| `agentos-vault` | AES-256-GCM encrypted secrets store |
| `agentos-sandbox` | Seccomp-BPF syscall filtering (Linux-only) |
| `agentos-pipeline` | Multi-step workflow orchestration engine |
| `agentos-hal` | Hardware Abstraction Layer |
| `agentos-wasm` | WASM tool execution via Wasmtime |
| `agentos-sdk` | Ergonomic macros and re-exports for tool development |

## Adding a New LLM Provider

1. Implement `LLMCore` in `crates/agentos-llm/src/`
2. Add to the `LLMProvider` enum in `crates/agentos-kernel/src/commands/agent.rs`
3. Wire up in the kernel provider selection logic

```rust
#[async_trait]
impl LLMCore for MyAdapter {
    async fn infer(&self, ctx: &ContextWindow, tools: &[ToolManifest]) -> Result<InferenceResult>;
    async fn infer_stream(&self, ctx: &ContextWindow, tools: &[ToolManifest]) -> Result<InferenceStream>;
    async fn health_check(&self) -> Result<bool>;
}
```

## Adding a New Tool

Use the `#[tool]` macro from `agentos-sdk`:
```rust
#[tool(name = "my-tool", description = "Does X", permissions = ["read"])]
async fn my_tool(input: MyInput) -> Result<ToolOutput> { ... }
```

Create a tool manifest at `tools/user/my-tool/TOOL.toml`.

## Code Conventions

- No `.unwrap()` in production paths — use `?` with `thiserror` errors
- All security operations must be logged to `AuditLog`
- File path inputs must reject `..` traversal
- Secrets must use `ZeroizingString`, never plain `String`
- Use `Arc<RwLock<T>>` for shared state
- Polymorphic adapters use `Arc<dyn Trait + Send + Sync>`
- Shutdown signaling via `CancellationToken` — propagate it, don't drop it

## Commit Style

```
feat(kernel): add priority scheduling for agent tasks
fix(audit): prevent duplicate Merkle chain entries
docs(cli): update provider list command help text
```

Prefix: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `chore`
Scope: crate name or area (`kernel`, `cli`, `llm`, `audit`, etc.)

## Development Workflow

1. Fork the repository and create a feature branch: `git checkout -b feat/my-feature`
2. Make your changes and write tests
3. Run the full check suite (see "Before You Submit")
4. Open a PR against `main`

Good first issues are labeled `good first issue` in the GitHub issue tracker.

## Reporting Security Issues

See [SECURITY.md](SECURITY.md) — do not open public issues for security vulnerabilities.
