# AgentOS — Comprehensive Project Overview

> A minimalist, LLM-native operating system written in Rust, designed for AI agents rather than humans.

**Core Philosophy**: LLMs are the CPU, tools are programs, intent is the syscall, security is non-negotiable.

---

## Executive Summary

AgentOS is a capability-based operating system for AI agents, implemented as a Rust workspace of **17 interconnected crates** (~67,000 lines of code). It features a Unix domain socket IPC architecture connecting a CLI (`agentctl`) to a high-performance inference kernel. The project is in **V3 development** as of March 2026, focused on production stability and advanced agentic workflows.

---

## System Architecture

```
CLI (agentctl)
    ↓ Unix domain socket (length-prefixed JSON)
Bus (IPC message transport)
    ↓
Kernel (inference orchestrator + subsystems)
    ├─ Scheduler, Router, Context Manager, Agent Registry
    ├─ Memory (episodic, semantic, procedural)
    ├─ Security (capability tokens, vault, audit log)
    ├─ Cost tracking, escalation, injection scanning
    └─ LLM adapters (Ollama, OpenAI, Anthropic, Gemini, Mock)
```

---

## Crate Structure (17 Crates)

| Crate | Purpose | Key Responsibility |
|-------|---------|-------------------|
| **agentos-types** | Shared type definitions | TaskID, AgentID, ToolID (UUID newtype wrappers), IntentMessage, AgentTask, ToolManifest, error types |
| **agentos-kernel** | Central orchestrator | Scheduler, router, context manager, task executor, cost tracker, escalation manager, injection scanner, risk classifier, snapshot manager, event bus |
| **agentos-cli** | Control interface | `agentctl` binary with 12+ command groups (agent, task, tool, secret, permission, role, audit, schedule, etc.) |
| **agentos-bus** | IPC transport | Unix domain socket server/client, length-prefixed JSON serialization, message routing |
| **agentos-llm** | LLM adapters | LLMCore trait + 5 implementations (Ollama, OpenAI, Anthropic, Gemini, Mock); streaming inference, token estimation, cost calculation |
| **agentos-tools** | Built-in tools | 40+ tools: file I/O (reader, writer, editor, delete, diff, move, glob, grep), memory (search, write, delete), shell (exec), data parsing, HTTP (GET/POST/PUT/DELETE with vault auth), hardware monitoring, task delegation, episodic memory, procedural memory, escalation status |
| **agentos-capability** | Security tokens | CapabilityToken (HMAC-SHA256 signed), PermissionSet (rwxqo flags), PermissionProfile, path-prefix matching, deny lists, SSRF blocking |
| **agentos-vault** | Encrypted secrets | AES-256-GCM encrypted secrets store, Argon2id key derivation, ZeroizingString for plaintext clearance |
| **agentos-audit** | Audit log | Append-only SQLite log with 83+ event types, HMAC chain verification for tamper detection, retention policies |
| **agentos-memory** | Multi-tier memory | Episodic store (per-agent event timeline), semantic store (vector embeddings + FTS5 hybrid search), procedural store (learned skills), ONNX embedder (MiniLM-L6-v2, 384-dim) |
| **agentos-sandbox** | Seccomp-BPF | Linux-only syscall filtering, sandboxed tool execution, resource limits (CPU, memory), network isolation |
| **agentos-wasm** | WASM runtime | Wasmtime 38 integration for tool execution, WASI support |
| **agentos-hal** | Hardware abstraction | 6 drivers: System (CPU/mem/uptime), Process (list/kill), Network (stats), LogReader, Sensor, GPU/Storage (planned) |
| **agentos-pipeline** | Workflow orchestration | YAML-defined multi-step workflows, topological dependency resolution, template substitution, retry/timeout |
| **agentos-web** | Web UI (WIP) | Axum 0.8 + HTMX dashboard, real-time SSE task streaming, pages for agents, tasks, tools, secrets, pipelines, audit |
| **agentos-sdk** | Tool development kit | Proc-macro `#[tool(...)]` for ergonomic WASM tool authoring, auto-manifest generation |
| **agentos-agent-tester** | Test harness | LLM-driven scenario evaluation, multi-turn feedback collection, report generation with consensus metrics |

---

## Intent Flow (How a Task Executes)

```
1. User: agentctl task run "analyze this data"
2. CLI: Serialize → KernelCommand::RunTask
3. Bus: Send over Unix socket
4. Kernel: Deserialize + route to agent
5. Router: Select target by strategy (name, task count, regex, random)
6. Scheduler: Enqueue task with budget/complexity tracking
7. TaskExecutor: Create ContextWindow, compile system prompt
8. LLM: infer_with_tools(context, tools, options)
9. Tool routing: Parse tool calls, check CapabilityToken signature
10. Tool execution: In-process (core/verified) or sandboxed (community)
11. Result injection: Inject tool output into context
12. Repeat 8-11 until agent calls intent.complete() or budget exhausted
```

---

## Boot Sequence (17 Steps)

The `Kernel::boot()` initializes in strict order:

1. Load config from TOML
2. Create directories (audit, vault, tools, bus socket)
3. Install core tool manifests
4. Open audit log DB
5. Unlock encrypted vault
6. Init capability engine + permission matrix
7. Init HAL with 6 drivers
8. Load + verify tool manifests (trust tier checking)
9. Build JSON schema registry
10. Init episodic, semantic, procedural memory stores
11. Register WASM tools via Wasmtime
12. Init scheduler, context manager, agent registry, router
13. Create pipeline engine
14. Start bus server
15. Init V3 systems (cost tracker, escalation, injection scanner, risk classifier, snapshot mgr, event bus)
16. Create internal notification channels (capacity 1024)
17. Emit `KernelStarted` audit event

After boot, **11 concurrent subsystem tasks** launch with fault-tolerant auto-restart: max 5 restarts per 60-second window, exponential backoff with jitter, circuit breaker protection.

---

## Security Model

### Capability Tokens

Every task receives a **CapabilityToken** (HMAC-SHA256 signed):
- **Scopes**: allowed tools, allowed intents, permissions, task lifetime
- **Unforgeable**: signed by kernel, verified on every tool call
- **Time-limited**: issued at task start, expires after `default_task_timeout_secs`
- **Enforcement**: Tools check token before running; rejected without valid signature

### Trust Tier System

| Tier | Behavior | Signature Required |
|------|----------|-------------------|
| Core | Distribution-trusted, no runtime sig check, in-process execution | No |
| Verified | Signed by known publisher, runtime verification | Ed25519 |
| Community | Untrusted, sandboxed execution, runtime verification | Ed25519 |
| Blocked | Hard-rejected by kernel | N/A |

### Permission System

- **PermissionSet**: Entries with rwxqo flags (read, write, execute, query, observe)
- **Path-prefix matching**: `"fs:/home/user/"` matches `"fs:/home/user/docs/file.txt"`
- **Deny entries**: E.g., `"~/.ssh/"` blocks all sensitive paths
- **SSRF protection**: Blocks private IP ranges (10.x, 172.16-31.x, 192.168.x, 127.x, ::1, fe80:)
- **Time-limited permissions**: Optional `expires_at` per entry
- **Default agent matrix**: `fs.user_data`, `memory.search`, `memory.write`, `tool.discovery`, `process.list`

### Secrets Management

- **Encryption**: AES-256-GCM with Argon2id key derivation
- **Scopes**: Agent-scoped, task-scoped, or global
- **Vault proxy**: Secret values never exposed to untrusted tools — proxy tokens used instead
- **Zero clearance**: `ZeroizingString` trait ensures plaintext is overwritten before drop

### Injection Scanning

- **Unicode homoglyph detection**: NFC normalization to catch spoofed characters
- **`<user_data>` tags**: Marked for inspection, not auto-trusted
- **Threat levels**: Low, Medium, High, Critical — blocks execution at High+
- **System prompt instruction**: Standing guidance to distrust `<user_data>` content

### Audit Logging

- **83+ event types**: AgentConnected, TaskStarted, ToolExecuted, BudgetExceeded, EscalationCreated, InjectionDetected, etc.
- **Append-only**: SQLite with HMAC chain verification for tamper detection
- **Retention**: Configurable max_audit_entries (0 = unlimited)
- **On-boot verification**: Chain check of last N entries

---

## Advanced Features

### Cost Tracking (V3)

- **Per-agent daily budgets**: Token count, cost (micro-USD), tool calls
- **Budget checks**: Before/after inference, hard/soft limits
- **Enforcement actions**:
  - Warning threshold (50%) → emit event
  - Pause threshold (80%) → suspend agent or downgrade model
  - Hard limit (100%) → BudgetAction (suspend, downgrade, shutdown)
- **Model downgrade**: Switch to cheaper model automatically (e.g., gpt-4o → gpt-4-turbo → gpt-3.5-turbo)
- **Persistence**: Snapshots stored in SQLite for crash recovery

### Escalation Management (V3)

- **Pending escalations**: Task context, decision point, options, urgency, blocking/non-blocking
- **Auto-deny after 5min**: Default behavior (configurable auto_action)
- **Resolution**: Human operator resolves via CLI (`agentctl escalation resolve`)
- **Notifications**: Optional webhook on creation
- **Persistence**: SQLite-backed for durability

### Memory System

| Tier | Purpose | Storage |
|------|---------|---------|
| Episodic | Timeline of events per agent, auto-write on task completion | SQLite |
| Semantic | Vector embeddings (ONNX MiniLM-L6-v2, 384-dim) + FTS5 hybrid search | SQLite + vectors |
| Procedural | Learned skills with preconditions/steps/postconditions | SQLite |

- **Consolidation**: Auto-summary after 100 task completions or 24h
- **Retrieval gate**: Adaptive ranking by relevance, recency, task context

### Context Window Management

- **Max entries**: Configurable (default 500)
- **Token budget**: 32K (80% compress, 95% checkpoint+flush)
- **Overflow strategy**: SemanticEviction (compress oldest entries with summary)
- **Pinned entries**: System prompt and recent task context stay
- **Entry importance**: 0.0-1.0 score for eviction ranking

### Pipeline Engine

- **YAML-defined workflows**: Multi-step with dependencies
- **Topological resolution**: Automatic execution ordering
- **Template substitution**: Variable passing between steps
- **Error handling**: Retry policies, timeouts, failure modes

### Subsystem Auto-Restart

- **11 concurrent subsystems**: Acceptor, Executor, TimeoutChecker, Scheduler, EventDispatcher, ToolLifecycleListener, CommNotificationListener, ScheduleNotificationListener, ArbiterNotificationListener, HealthMonitor, Consolidation
- **Restart budget**: Max 5 restarts per 60-second window
- **Backoff**: `min(base * 2^attempt + jitter, max_delay)` where base=500ms, max=30s
- **Circuit breaker**: After budget exhaustion, subsystem stays down until window resets
- **Critical subsystems**: Acceptor, Executor, TimeoutChecker, EventDispatcher trigger full kernel shutdown if unrecoverable

---

## Configuration

Default config at `config/default.toml`. Key settings:

| Section | Setting | Default |
|---------|---------|---------|
| Kernel | Concurrent tasks | 4 |
| Kernel | Default timeout | 3600s |
| Kernel | Sandbox policy | `trust_aware` (core=in-process, community=sandboxed) |
| Autonomous | Max iterations | 10,000 |
| Autonomous | Timeout | 24h |
| LLM | Provider | Ollama (localhost:11434) |
| LLM | Context window | 32K |
| LLM | Request timeout | 300s |
| Memory | Embeddings | Enabled |
| Memory | Consolidation trigger | 100 completions or 24h |
| Context Budget | Total tokens | 128K |
| Context Budget | Reserve | 25% |
| Context Budget | Allocations | System 15%, Tools 18%, Knowledge 30%, History 25%, Task 12% |
| Logging | Format | JSON (prod), text (dev) |
| Logging | Retention | 7 days rolling |
| Vault | Encryption | AES-256-GCM |
| Audit | Storage | SQLite, unlimited entries |

---

## Task Routing Strategies

| Strategy | Behavior |
|----------|----------|
| Round-robin | Cycles through agents fairly |
| Least-loaded | Picks agent with fewest active tasks |
| Name-exact | Routes to agent by explicit name |
| Regex | Matches agent names against patterns |

---

## Web UI (agentos-web)

Built with **Axum 0.8 + HTMX 2.x + Alpine.js + Pico CSS v2.1.1**:

- Real-time SSE task streaming
- Dashboard pages: agents, tasks, tools, secrets, pipelines, audit
- Template engine: MiniJinja2 (semantic HTML, classless CSS)
- Security: CSRF protection, CORS, CSP, rate limiting (in progress)

---

## Technology Stack

| Area | Technology | Notes |
|------|-----------|-------|
| Language | Rust | 2021 edition |
| Async runtime | Tokio | Full features |
| Serialization | Serde + serde_json | Standard format |
| Crypto | HMAC-SHA256, AES-256-GCM, Ed25519, Argon2id | hmac, sha2, aes-gcm, ed25519-dalek, argon2 |
| Database | SQLite | rusqlite 0.31, bundled |
| CLI | Clap 4 | Derive macros |
| HTTP client | Reqwest 0.12 | rustls-tls, streaming |
| Web framework | Axum 0.8 | Web UI |
| WASM runtime | Wasmtime 38 | Async + cranelift |
| Logging | Tracing | JSON + text, rolling appender |
| Embeddings | ONNX Runtime | MiniLM-L6-v2, 384-dim vectors |
| Template engine | MiniJinja | HTMX + Alpine.js frontend |
| System info | sysinfo 0.33 | HAL drivers |

### Platform Support

- **Primary**: Linux x86_64 (full seccomp support)
- **Seccomp**: Linux-only, gated behind `#[cfg(target_os = "linux")]`
- **Portability**: Async-first design allows macOS/Windows builds (minus sandbox)

---

## Current Development Phase (V3)

### Completed
- HTTP client tool
- HAL + system tools
- Memory upgrade (vector embeddings)
- Multi-agent pipelines
- Agent tester harness
- Trust tier system with Ed25519 signing
- Cost tracking with budget enforcement
- Escalation management
- Injection scanning
- Resource arbitration
- Subsystem restart hardening

### In Progress
- WebUI security fixes (CSRF, CORS, CSP, rate limiting)
- Agent experience and workflow improvements

### Planned
- Agent Scratchpad — Obsidian-inspired knowledge graph for agent working memory (6 phases)
- Production stability fixes (8 days effort)
- Docker deployment infrastructure
- Rust SDK proc-macros refinement

### Known Gaps
1. HAL per-device quarantine/approval workflow (only registry exists)
2. Zero-exposure secret proxy at tool exec boundary (proxy tokens exist, not wired)
3. Persistent Tier 3 memory on-disk (only in-memory tiers exist)
4. Episodic memory auto-write on task completion

---

## Project Metrics

| Metric | Value |
|--------|-------|
| Total lines of code | ~67,000 |
| Crate count | 17 |
| Built-in tools | 40+ |
| Audit event types | 83+ |
| LLM adapters | 5 |
| Trust tiers | 4 |
| Memory tiers | 3 |
| Routing strategies | 4 |
| Config sections | 12+ |
| Subsystem tasks | 11 |
| CLI command groups | 12+ |

---

## Developer Quick Reference

### Build & Test
```bash
cargo build --workspace          # Build all crates
cargo test --workspace           # Run all tests
cargo clippy --workspace -- -D warnings  # Lint (CI-enforced)
cargo fmt --all -- --check       # Format check (CI-enforced)
cargo test -p agentos-kernel     # Test specific crate
```

### Adding a New Tool
```rust
#[tool(name = "my-tool", description = "...", permissions = "fs.read")]
async fn my_tool(input: serde_json::Value) -> Result<serde_json::Value> { ... }
```

### Adding a New LLM Adapter
```rust
#[async_trait]
impl LLMCore for MyAdapter {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult>;
    async fn infer_with_tools(&self, ctx: &ContextWindow, tools: &[ToolManifest]) -> Result<InferenceResult>;
    async fn health_check(&self) -> Result<bool>;
}
```

### Key Files
- `CLAUDE.md` — Project instructions and conventions
- `Cargo.toml` — Workspace definition
- `config/default.toml` — Kernel configuration
- `crates/agentos-kernel/src/run_loop.rs` — Main event loop
- `crates/agentos-kernel/src/task_executor.rs` — Task execution engine
- `crates/agentos-types/src/` — Authoritative type definitions
- `obsidian-vault/` — Living documentation
