# Architecture Deep Dive

This document explains how all the pieces of AgentOS fit together — from the kernel down to individual tool executions.

---

## System Architecture

```
┌──────────────────────────────────────────────────────────┐
│                      AgentOS                              │
│                                                          │
│  ┌──────────────┐                                        │
│  │  agentctl    │  (CLI — clap-based)                    │
│  │  CLI Client  │                                        │
│  └──────┬───────┘                                        │
│         │ Unix Domain Socket (Intent Bus)                 │
│  ┌──────▼──────────────────────────────────────────────┐ │
│  │              Inference Kernel                        │ │
│  │                                                     │ │
│  │  ┌────────────────┐  ┌────────────────────────────┐ │ │
│  │  │ Task Scheduler │  │ Capability Engine           │ │ │
│  │  │  (priority Q)  │  │  (HMAC tokens + perm matrix)│ │ │
│  │  ├────────────────┤  ├────────────────────────────┤ │ │
│  │  │ Context Mgr    │  │ Secrets Vault              │ │ │
│  │  │  (per-task)    │  │  (AES-256-GCM + Argon2id)  │ │ │
│  │  ├────────────────┤  ├────────────────────────────┤ │ │
│  │  │ Audit Log      │  │ Agent Registry             │ │ │
│  │  │  (append-only) │  │  (profiles + status)       │ │ │
│  │  ├────────────────┤  ├────────────────────────────┤ │ │
│  │  │ Task Router    │  │ Agent Message Bus          │ │ │
│  │  │  (multi-LLM)   │  │  (direct + broadcast)     │ │ │
│  │  ├────────────────┤  ├────────────────────────────┤ │ │
│  │  │ Schedule Mgr   │  │ Background Pool            │ │ │
│  │  │  (cron jobs)   │  │  (detached tasks)          │ │ │
│  │  └────────────────┘  └────────────────────────────┘ │ │
│  └──────┬──────────────────────┬───────────────────────┘ │
│         │                      │                          │
│  ┌──────▼──────────┐   ┌──────▼────────────────────────┐ │
│  │ LLM Adapters    │   │ Tool Registry + Sandbox       │ │
│  │                 │   │                               │ │
│  │  Ollama         │   │  file-reader    data-parser   │ │
│  │  OpenAI         │   │  file-writer    shell-exec    │ │
│  │  Anthropic      │   │  memory-search  agent-message │ │
│  │  Gemini         │   │  memory-write   task-delegate │ │
│  │  Custom         │   │                               │ │
│  └─────────────────┘   └───────────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

---

## Crate Dependency Graph

AgentOS is organized as a Rust workspace with 17 crates. Dependencies flow strictly downward — no circular dependencies.

```
agentos-cli
    ├── agentos-kernel
    │       ├── agentos-llm
    │       │       └── agentos-types
    │       ├── agentos-bus
    │       │       └── agentos-types
    │       ├── agentos-tools
    │       │       └── agentos-types
    │       ├── agentos-capability
    │       │       └── agentos-types
    │       ├── agentos-vault
    │       ├── agentos-audit
    │       │       └── agentos-types
    │       ├── agentos-sandbox
    │       └── agentos-types
    └── agentos-bus
```

### Crate Descriptions

| Crate                  | Purpose                                 | Key Exports                                                                                                                                        |
| ---------------------- | --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| **agentos-types**      | Shared type definitions used everywhere | `IntentMessage`, `CapabilityToken`, `AgentTask`, `ContextWindow`, `ToolManifest`, `AgentProfile`, `SecretEntry`, `Role`                            |
| **agentos-audit**      | Append-only immutable audit log         | `AuditLog` (SQLite-backed)                                                                                                                         |
| **agentos-vault**      | Encrypted credential storage            | `SecretsVault`, `MasterKey`, `ZeroizingString`                                                                                                     |
| **agentos-capability** | Permission system and token management  | `CapabilityEngine`, `PermissionProfile`, `ProfileManager`                                                                                          |
| **agentos-bus**        | IPC layer over Unix domain sockets      | `BusServer`, `BusClient`, `BusConnection`, `KernelCommand`, `KernelResponse`                                                                       |
| **agentos-llm**        | LLM adapter trait and implementations   | `LLMCore` trait, `OllamaCore`, `OpenAICore`, `AnthropicCore`, `GeminiCore`, `CustomCore`                                                           |
| **agentos-tools**      | Built-in tool implementations (41 tools) | `AgentTool` trait, file tools, memory tools, procedural tools, network tools, agent coordination, utilities |
| **agentos-sandbox**    | Seccomp-BPF sandboxed process execution | `SandboxExecutor`, `SandboxConfig`, `SandboxResult`                                                                                                |
| **agentos-memory**     | Multi-tier memory (semantic + episodic) with embeddings | `SemanticMemory`, `EpisodicMemory`, `ProceduralMemory`                                                                               |
| **agentos-pipeline**   | Multi-step workflow orchestration engine | `PipelineEngine`, `PipelineStore`, template variable sanitization                                                                                  |
| **agentos-hal**        | Hardware Abstraction Layer (system, process, network, GPU, storage, sensor) | `HalRegistry`, driver traits                                                                              |
| **agentos-sdk**        | Ergonomic macros and re-exports for tool development | `#[tool]` attribute macro, re-exports from `agentos-types` and `agentos-tools`                                                                    |
| **agentos-sdk-macros** | Proc-macro crate for `#[tool]` attribute | `tool` attribute macro                                                                                                                            |
| **agentos-wasm**       | WASM tool execution via Wasmtime        | `WasmExecutor`, `WasmModule`                                                                                                                       |
| **agentos-web**        | Web UI server (Axum + HTMX)            | `WebServer`, task/agent/audit views, chat interface                                                                                                |
| **agentos-kernel**     | Central orchestrator — the "brain"      | `Kernel`, `TaskScheduler`, `ContextCompiler`, `AgentRegistry`, `ToolRegistry`, `TaskRouter`, `CostTracker`, `IntentValidator`, `EventDispatch`     |
| **agentos-cli**        | User-facing CLI (`agentctl`)            | `Cli`, `Commands`, all command handlers                                                                                                            |

---

## Kernel Boot Sequence

When you run `agentctl start`, the following happens:

```
1. Parse CLI arguments and load config (config/default.toml)
   - Validate all config sections (task_limits, context_budget, LLM, workspace paths)
2. Prompt for vault passphrase (if not provided via --vault-passphrase)
3. Initialize subsystems:
   a. AuditLog      — open/create SQLite database
   b. Audit verify   — verify hash chain integrity of last N entries (configurable)
   c. SecretsVault  — derive master key from passphrase via Argon2id, open encrypted vault DB
   d. CapabilityEngine — initialize with signing key from vault
   e. ToolRegistry  — scan core_tools_dir and user_tools_dir, load all .toml manifests
   f. AgentRegistry — load persisted state from disk (with backup recovery)
   g. TaskScheduler — initialize priority queue
   h. CostTracker   — initialize per-agent budget tracking
   i. ContextCompiler — initialize with token budget config
   j. TaskRouter    — load routing strategy from config
   k. EventDispatch — initialize event broadcast channel
   l. ScheduleManager — initialize cron scheduler
   m. BackgroundPool — initialize background task pool
   n. AgentMessageBus — initialize message channels
   o. StateStore    — open/create SQLite for persisted kernel state
   p. HealthMonitor — initialize system health checks
4. Start BusServer on Unix domain socket
5. Enter main run loop — accept connections, dispatch commands
```

---

## Intent Flow: From Prompt to Result

This is what happens when you run a task:

```
agentctl task run --agent analyst "Read file.txt and summarize it"
         │
         ▼
[1] CLI serializes → KernelCommand::RunTask { agent_name, prompt }
         │
         ▼
[2] Bus Client sends command over Unix socket
         │
         ▼
[3] Kernel receives, routes to cmd_run_task()
         │
         ▼
[4] Agent lookup — verify agent exists and is Online/Idle
         │
         ▼
[5] Create AgentTask with unique TaskID
    - Initialize ContextWindow with system prompt + agent directory
    - Issue CapabilityToken (HMAC-signed, scoped to agent's permissions)
         │
         ▼
[6] Send prompt + context to LLM via adapter (e.g. OllamaCore::infer())
         │
         ▼
[7] LLM responds — may include tool calls embedded in the response
         │
         ▼
[8] parse_tool_call() — detect tool invocations in LLM output
         │
         ▼
[9] For each tool call:
    a. Verify tool exists in ToolRegistry
    b. Check CapabilityToken — does agent have required permissions?
    c. Execute tool via ToolRunner (sandboxed if applicable)
    d. Inject tool result back into ContextWindow
    e. Re-send updated context to LLM
         │
         ▼
[10] LLM produces final answer (no more tool calls)
         │
         ▼
[11] Result returned to CLI via Bus → displayed to user
         │
         ▼
[12] Entire interaction logged to AuditLog
```

---

## Task Routing Engine

When no specific agent is requested, the kernel's **TaskRouter** selects the best available agent.

### Routing Strategies

| Strategy           | Description                                                              |
| ------------------ | ------------------------------------------------------------------------ |
| `capability-first` | Prefers cloud LLMs (Anthropic > OpenAI > Gemini > Custom > Ollama)       |
| `cost-first`       | Prefers local/cheap LLMs (Ollama > Custom > Gemini > OpenAI > Anthropic) |
| `latency-first`    | Same heuristic as cost-first (local models are typically faster)         |
| `round-robin`      | Distributes tasks evenly across all online agents                        |

### Routing Rules

Rules are evaluated before the strategy. Each rule can match a task prompt via regex:

```
Task prompt → Match against rules (regex)
    │
    ├── Rule matches? → Use preferred_agent (or fallback_agent if offline)
    │
    └── No rules match? → Apply routing strategy
```

---

## Memory Architecture

AgentOS manages three tiers of memory:

### Tier 1: Working Memory (per-task, in-memory)

- Active context window — ring buffer of conversation entries
- Includes: system prompt, agent directory, task history, tool results
- When the window overflows, oldest entries are summarized or evicted
- Managed by `ContextManager`

### Tier 2: Episodic Memory (per-task, persisted)

- Full task history persisted in SQLite
- Intent messages, tool calls, LLM responses — indexed for recall
- Each task gets its own episodic record
- Managed by `EpisodicMemory`

### Tier 3: Semantic Memory (global, persisted)

- Long-term recall via hybrid vector + FTS5 search
- Cross-task, cross-agent, cross-session
- Accessed via `memory-search`, `memory-write`, `memory-read`, `memory-delete` tools
- Permission-gated: agents can have different read/write scopes

### Tier 4: Procedural Memory (global, persisted)

- Reusable step-by-step procedures extracted from successful tasks
- Can be created manually by agents (`procedure-create`) or auto-populated by the consolidation engine
- Searched by natural language query via `procedure-search`
- The kernel auto-queries procedures at task start and injects relevant ones into context

---

## Agent Message Bus

The Agent Message Bus enables agent-to-agent communication:

| Mode                | Description                                   |
| ------------------- | --------------------------------------------- |
| **Direct Message**  | One agent sends a message to a specific agent |
| **Task Delegation** | An agent hands off a subtask to another agent |
| **Broadcast**       | An agent sends a message to all active agents |

### Security Rules

- Sending requires `agent.message:x` permission
- Receiving requires `agent.message:r`
- Delegated tasks receive strictly downscoped tokens (child can never have more permissions than parent)
- All messages are logged to the immutable audit log
- Agents cannot impersonate each other — messages are signed with kernel-issued identity tokens
