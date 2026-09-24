# Contributing to AgentOS

Thank you for your interest in contributing to AgentOS!

## Quick Start

```bash
git clone https://github.com/AjasMohammed/Agos.git
cd agentos
cargo build --workspace
cargo test --workspace
```

## Your First 30 Minutes

The full workspace is ~300k lines across 29 crates. You do not need to build all of it to land a fix.

```bash
# 1. Build just the CLI binary (pulls in the kernel). ~13 GB RAM is enough with -j 4.
cargo build -p agentos-cli -j 4

# 2. Run one small crate's tests to confirm the toolchain works.
cargo test -p agentos-types

# 3. See what tools exist: one TOML manifest per built-in tool.
ls tools/core

# 4. See how integration tests are laid out. Security regressions live in
#    tests/security_*.rs (one file per public vulnerability class).
ls crates/agentos-kernel/tests/

# 5. Run the security suite on its own.
cargo test -p agentos-kernel --test security_acceptance_test --test security_ssrf_metadata_blocked \
  --test security_injected_config_write_requires_approval --test security_file_tools_reject_traversal
```

Adding a security test: copy the smallest existing `security_*.rs`, name the file after the attack class, include one negative case and one positive control, and add a row to the "Threat Classes We Test Against" table in `docs/guide/06-security.md`.

Scope freeze: until `v1.0.0` is tagged we accept bug fixes, security fixes, tests, and docs. New channels, providers, tools, or endpoints wait; open an issue first so the idea is not lost.

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
