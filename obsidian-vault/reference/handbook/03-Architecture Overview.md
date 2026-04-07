---
title: Architecture Overview
tags:
  - docs
  - handbook
date: 2026-03-16
status: complete
---

# Architecture Overview

> A deep dive into the AgentOS kernel, crate dependencies, boot sequence, intent flow, routing, memory, events, security, and cost tracking.

---

## System Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                          agentos (CLI)                             │
│                     clap-based, 17+ commands                        │
└──────────────────────────────┬──────────────────────────────────────┘
                               │ Unix Domain Socket (length-prefixed JSON)
                               │
┌──────────────────────────────▼──────────────────────────────────────┐
│                         agentos-bus                                  │
│                    IPC Message Transport                             │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
┌─────────────────────────────────────────────────────────────────────┐
│                       agentos-api (optional)                        │
│                  REST API + WebSocket Server                         │
│                                                                     │
│  GET/POST /api/v1/*  │  GET /api/v1/ws?token=  │  API Key Auth      │
│  30+ REST endpoints  │  Real-time events, chat  │  Bearer tokens     │
│                      │  streaming, task control  │  HMAC-SHA256 keys  │
│                 KernelService trait (abstraction layer)              │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
┌──────────────────────────────▼──────────────────────────────────────┐
│                      INFERENCE KERNEL                                │
│                                                                      │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────┐  ┌───────────┐ │
│  │  Scheduler   │  │   Router    │  │   Context     │  │  Agent    │ │
│  │  (tasks)     │  │  (4 strats) │  │   Manager     │  │  Registry │ │
│  └─────────────┘  └─────────────┘  └──────────────┘  └───────────┘ │
│                                                                      │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────┐  ┌───────────┐ │
│  │  Cost        │  │ Escalation  │  │  Injection    │  │   Risk    │ │
│  │  Tracker     │  │  Manager    │  │  Scanner      │  │ Classifier│ │
│  └─────────────┘  └─────────────┘  └──────────────┘  └───────────┘ │
│                                                                      │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────┐  ┌───────────┐ │
│  │  Event Bus   │  │  Snapshot   │  │  Pipeline     │  │  Intent   │ │
│  │              │  │  Manager    │  │  Engine       │  │ Validator │ │
│  └─────────────┘  └─────────────┘  └──────────────┘  └───────────┘ │
│                                                                      │
└──┬──────────┬──────────┬──────────┬──────────┬──────────┬───────────┘
   │          │          │          │          │          │
   ▼          ▼          ▼          ▼          ▼          ▼
┌──────┐ ┌──────┐ ┌──────────┐ ┌───────┐ ┌────────┐ ┌────────┐
│ LLM  │ │Tools │ │ Security │ │Memory │ │ Audit  │ │  HAL   │
│      │ │      │ │          │ │       │ │        │ │        │
│Ollama│ │file  │ │Capability│ │Episod.│ │SQLite  │ │System  │
│OpenAI│ │shell │ │  Vault   │ │Semant.│ │85+ evt │ │Process │
│Anthro│ │memory│ │ Sandbox  │ │Proced.│ │  types │ │Network │
│Gemini│ │data  │ │  WASM    │ │Embedd.│ │        │ │GPU     │
│Mock  │ │coord │ │          │ │       │ │        │ │Storage │
│      │ │      │ │          │ │       │ │        │ │Audio*  │
└──────┘ └──────┘ └──────────┘ └───────┘ └────────┘ └────────┘
```

---

## Crate Dependency Graph

The 18 crates form a layered dependency tree. Dependencies flow downward — no circular dependencies.

```
agentos-cli
├── agentos-kernel
│   ├── agentos-types          (shared types, IDs, errors)
│   ├── agentos-bus            (IPC messages, bus server)
│   │   └── agentos-types
│   ├── agentos-llm            (LLM adapters)
│   │   └── agentos-types
│   ├── agentos-tools          (built-in tools, signing)
│   │   ├── agentos-types
│   │   └── agentos-capability
│   ├── agentos-capability     (tokens, permissions)
│   │   └── agentos-types
│   ├── agentos-vault          (encrypted secrets)
│   │   └── agentos-types
│   ├── agentos-audit          (audit log)
│   │   └── agentos-types
│   ├── agentos-memory         (episodic, semantic, procedural)
│   │   └── agentos-types
│   ├── agentos-pipeline       (workflow orchestration)
│   │   └── agentos-types
│   ├── agentos-sandbox        (seccomp-BPF)
│   │   └── agentos-types
│   ├── agentos-wasm           (Wasmtime runtime)
│   │   └── agentos-types
│   └── agentos-hal            (hardware abstraction)
│       └── agentos-types
├── agentos-bus
└── agentos-types

agentos-api                    (REST API + WebSocket server)
├── agentos-kernel             (via KernelService trait impl)
└── agentos-types

agentos-sdk                    (tool development kit)
├── agentos-sdk-macros         (proc-macro for #[tool])
└── agentos-types

agentos-web                    (web UI, under development)
├── agentos-kernel
└── agentos-types
```

---

## Kernel Boot Sequence

When `agentos start` is called, `Kernel::boot()` performs these steps in order:

| Step | Subsystem | What Happens |
|------|-----------|--------------|
| 1 | Config | Load configuration from TOML file |
| 2 | Directories | Create directories for audit, vault, tools, and bus socket |
| 3 | Tools | Install core tool manifests from `tools/core/` |
| 4 | Audit | Open SQLite audit log database, create tables if needed |
| 5 | Vault | Open encrypted secrets vault, derive key with Argon2id from passphrase |
| 6 | Capability | Initialize the capability engine and load permission matrix |
| 7 | HAL | Initialize Hardware Abstraction Layer with core drivers (System, Process, Network, Sensor, GPU, Storage, Log Reader) plus feature-gated peripheral drivers (Audio, Bluetooth, Display, Printer, Raw USB, USB Storage, Webcam) |
| 8 | Tools | Load tool manifests, validate trust tiers (Core/Verified/Community/Blocked), check Ed25519 signatures |
| 9 | Schema | Build JSON schema registry from tool manifests for intent validation |
| 10 | Memory | Initialize embedder and 3 memory stores: episodic, semantic, procedural |
| 11 | WASM | Register WASM-based tools from manifests via Wasmtime runtime |
| 12 | Core | Initialize scheduler, context manager, agent registry, and task router |
| 13 | Pipeline | Create pipeline engine for multi-step workflow orchestration |
| 14 | Bus | Start bus server listening on Unix domain socket for CLI commands |
| 15 | V3 Systems | Initialize cost tracker, escalation manager, injection scanner, risk classifier, snapshot manager, event bus |
| 16 | IPC | Create bounded channels (capacity 1024) for internal subsystem communication |
| 17 | API | If `[api].enabled = true`, start `agentos-api` HTTP server alongside the kernel (REST + WebSocket) |
| 18 | Audit | Emit `KernelStarted` audit event — system is ready |

After boot, the kernel enters the main event loop (`run_loop.rs`) which spawns 9 concurrent subsystem tasks, each with fault-tolerant auto-restart (max 5 restarts per 60-second window).

---

## Intent Flow

When a user issues a CLI command that triggers LLM inference, the request flows through 12 steps:

```
 1. User types CLI command
    │
 2. agentos parses command, serializes to BusMessage
    │
 3. BusMessage sent over Unix domain socket to kernel
    │
 4. Kernel deserializes → KernelCommand
    │
 5. Router selects target agent (strategy + rules)
    │
 6. CapabilityToken validated against required PermissionSet
    │
 7. Intent schema validated against tool's JSON Schema
    │
 8. Injection scanner checks prompt for known attack patterns
    │
 9. Tool execution in sandbox (seccomp-BPF or WASM)
    │
10. Tool result sanitized and injected into ContextWindow
    │
11. LLM inference with context → InferenceResult
    │
12. AuditLog entry written, result returned via bus to CLI
```

### Step details

1. **CLI parsing** — `agentos` uses clap to parse arguments into a strongly-typed `Commands` enum
2. **Bus serialization** — the command is wrapped in a `BusMessage` with length-prefixed JSON encoding
3. **Socket transport** — sent over the Unix domain socket at the configured `bus.socket_path`
4. **Kernel dispatch** — `run_loop.rs` routes the message to the appropriate command handler in `commands/`
5. **Agent routing** — the `TaskRouter` evaluates pattern-based rules first, then falls back to the configured routing strategy (see [[#Task Routing Engine]])
6. **Capability check** — the agent's `CapabilityToken` is validated: HMAC signature, expiry, and required permissions
7. **Schema validation** — the intent payload is validated against the tool's JSON Schema definition
8. **Injection scan** — the `InjectionScanner` checks for prompt injection patterns and assigns a risk score
9. **Sandboxed execution** — tools run under seccomp-BPF syscall filtering (Linux) or WASM isolation
10. **Context injection** — tool results are sanitized (escape delimiters), wrapped in typed containers, and assigned importance scores (errors: 0.8, success: 0.5)
11. **LLM inference** — the `ContextWindow` is sent to the selected LLM adapter; response is parsed into `InferenceResult`
12. **Audit + response** — an audit entry is written to the append-only log; the result is serialized back to the CLI via the bus

---

## Task Routing Engine

The `TaskRouter` selects which agent handles a given task. It first evaluates **routing rules** (regex pattern matching on the prompt), then falls back to the configured **routing strategy**.

### Routing strategies

| Strategy | Preference Order | Use Case |
|----------|-----------------|----------|
| **CapabilityFirst** (default) | Anthropic → OpenAI → Gemini → Custom → Ollama | Maximum reasoning quality |
| **CostFirst** | Ollama → Custom → Gemini → OpenAI → Anthropic | Minimize cost, prefer local |
| **LatencyFirst** | Ollama → Custom → Gemini → OpenAI → Anthropic | Minimize response time (local = faster) |
| **RoundRobin** | Even distribution across all agents | Load balancing |

### Routing rules

Rules are evaluated before strategies. Each rule has:

| Field | Type | Description |
|-------|------|-------------|
| `task_pattern` | `Option<String>` | Regex pattern matched against the task prompt |
| `preferred_agent` | `String` | Primary agent to route to |
| `fallback_agent` | `Option<String>` | Backup agent if preferred is offline |

The router filters to **online and idle** agents only. If no online agent matches, it returns an error.

---

## Memory Architecture

AgentOS provides a 3-tier memory system, each tier serving a different temporal scope:

```
┌─────────────────────────────────────────────────────┐
│                  WORKING MEMORY                      │
│            (ContextWindow per task)                   │
│                                                      │
│  System prompt │ Tool results │ History │ Knowledge  │
│  Budget: token_budget from config (default 8000)     │
│  Eviction: semantic importance scoring               │
│  Compress at 80%, checkpoint at 95%                  │
└──────────────────────────┬──────────────────────────┘
                           │ task completion
                           ▼
┌─────────────────────────────────────────────────────┐
│                 EPISODIC MEMORY                       │
│               (EpisodicStore)                         │
│                                                      │
│  Task-scoped history: intents, tool calls, results   │
│  Auto-written on task completion                     │
│  Queryable by task ID, agent ID, time range          │
└──────────────────────────┬──────────────────────────┘
                           │ consolidation (hourly)
                           ▼
┌─────────────────────────────────────────────────────┐
│                 SEMANTIC MEMORY                       │
│               (SemanticStore)                         │
│                                                      │
│  Cross-task knowledge with vector embeddings         │
│  Keyword + similarity search                         │
│  Long-term knowledge base for all agents             │
└─────────────────────────────────────────────────────┘
```

Additionally, a **Procedural Memory** store (`ProceduralStore`) holds reusable how-to procedures and multi-step workflows that agents can retrieve and execute.

### Memory consolidation

The kernel runs a background consolidation process on a configurable interval (`memory.consolidation_interval_secs`, default 3600s) that extracts key information from episodic records and indexes it into the semantic store with embeddings for future retrieval.

---

## Agent Message Bus

Agents communicate via 3 messaging modes:

| Mode | Description | Use Case |
|------|-------------|----------|
| **Direct** | Point-to-point message from one agent to another | Asking a specific agent for help |
| **Delegation** | Assign a sub-task to another agent, await result | Complex tasks requiring specialized agents |
| **Broadcast** | Send a message to all registered agents | Announcements, shared state updates |

Messages flow through the kernel's `CommNotificationListener` subsystem, which validates capability tokens before delivery.

---

## Multi-Agent Coordination

AgentOS supports hierarchical multi-agent workflows through sub-agent spawning, context handoff, and agent teams.

### Architecture

```
Parent Agent Task
  │
  ├── spawn-agent → Child Task 1 (spawn_depth=1)
  │                   ├── ContextSlice (last N messages from parent)
  │                   └── Scoped CapabilityToken (intersection of parent perms)
  │
  ├── spawn-agent → Child Task 2 (spawn_depth=1)
  │
  └── await-agents [child1, child2]
        ├── SubAgentResult injected into parent context
        └── Parent resumes with children's outputs
```

### Key Components

| Component | Location | Role |
|-----------|----------|------|
| `ContextSlice` | `agentos-types/src/context.rs` | Portable slice of parent context for child handoff |
| `SubAgentResult` | `agentos-types/src/context.rs` | Completed child result, ready for parent injection |
| `ContextManager.seed_from_slice()` | `agentos-kernel/src/context.rs` | Seeds child context from parent slice |
| `ContextManager.inject_sub_agent_result()` | `agentos-kernel/src/context.rs` | Injects child result into parent context |
| `SpawnAgentTool` | `agentos-tools/src/coordination.rs` | Tool that emits `_kernel_action: "spawn_agent"` |
| `AwaitAgentsTool` | `agentos-tools/src/coordination.rs` | Tool that emits `_kernel_action: "await_agents"` |
| `VerifyOutputTool` | `agentos-tools/src/coordination.rs` | Spawns a critic agent for output verification |
| `TeamConfig` | `agentos-types/src/team.rs` | TOML-loadable team configuration |

### Safety Mechanisms

- **Spawn depth limit** — prevents unbounded recursive spawning (default max: 4)
- **Capability scoping** — child tokens are an intersection of parent permissions via `scope_for_child()`
- **Cascading cancellation** — parent cancellation propagates to all children
- **Idempotent injection** — `injected_sub_agents` set prevents duplicate result injection
- **Context message cap** — `context_messages` is clamped to 100 even if schema validation is bypassed

See [[05-Agent Management]] for the full user-facing reference.

---

## Event System Architecture

The event system enables reactive workflows where events trigger automated actions.

### Components

```
Event Source (tool exec, task complete, etc.)
    │
    ▼
EventBus (subscription registry + filter evaluator)
    │
    ├── Subscription 1: filter=[agent_id=X, event=TaskCompleted]
    │       → triggered task: "summarize results"
    │
    ├── Subscription 2: filter=[event=BudgetWarning]
    │       → triggered task: "notify admin"
    │
    └── Subscription 3: filter=[event=*] (throttled: 1/min)
            → triggered task: "log to external system"
```

### Subscription filtering

Each subscription has an `EventFilterExpr` composed of AND-combined predicates:

| Filter Operation | Description |
|-----------------|-------------|
| `Eq` | Exact match |
| `NotEq` | Not equal |
| `Gt` / `Gte` | Greater than / greater-or-equal |
| `Lt` / `Lte` | Less than / less-or-equal |
| `In` | Value is in a list |
| `Contains` | String contains substring |

Filter values can be `String`, `Number`, `Bool`, or `List`.

### Throttling

Subscriptions support rate limiting to prevent event storms:

- **Time-based**: minimum interval between deliveries
- **Count-based**: maximum deliveries per time window
- **Chain depth limit**: prevents infinite event → task → event loops (configurable `max_chain_depth`)

### Architecture note

The `EventBus` is a **pure registry and filter evaluator** — it does not create tasks directly. The kernel orchestrates the full flow: event emission → filter evaluation → task creation, via the `event_dispatch.rs` module.

---

## REST API and WebSocket Layer

The `agentos-api` crate provides an optional HTTP server that runs alongside the kernel when `[api].enabled = true` in the configuration. It exposes the kernel's full functionality to external consumers without requiring a Unix domain socket connection.

### Architecture

```
External Consumers (scripts, UIs, CI, other services)
      │
      │  HTTP/WebSocket  (Bearer agos_<key>)
      ▼
┌─────────────────────────────────────────────────────┐
│              agentos-api (Axum)                      │
│                                                     │
│  REST routes           WebSocket                    │
│  /api/v1/*             /api/v1/ws?token=            │
│                                                     │
│  Auth middleware        WsBroadcaster               │
│  (ApiKeyStore)          (event fan-out)             │
│                                                     │
│          KernelService trait                        │
│    (abstraction — delegates to Kernel)              │
└───────────────────────┬─────────────────────────────┘
                        │
                   Kernel (agentos-kernel)
```

### KernelService Trait

The `KernelService` trait in `agentos-api/src/service.rs` defines the complete API surface — agents, tasks, tools, secrets, pipelines, audit, costs, notifications, system status. The `Kernel` struct implements this trait in `kernel_impl.rs`, which translates REST DTOs into `KernelCommand` variants and dispatches them through the same `api_*` wrapper methods used by the bus path.

This design means:

- REST and CLI bus paths share identical kernel logic (no code duplication)
- The `KernelService` trait can be mocked for integration testing the API layer without a running kernel
- Additional transports (gRPC, GraphQL) can be added by implementing `KernelService`

### WebSocket Broadcaster

The `WsBroadcaster` is a multi-producer, multi-consumer event fan-out component wired into the kernel's internal event bus. When the kernel emits events (task completed, agent connected, budget alert, etc.), the broadcaster routes them to all active WebSocket sessions that have subscribed to the corresponding channel.

---

## Security Layers

AgentOS implements defense-in-depth with 8 security layers:

| Layer | Component | Mechanism |
|-------|-----------|-----------|
| **1. Capability Tokens** | `agentos-capability` | HMAC-SHA256 signed tokens with expiry, permission sets, and deny entries |
| **2. Permission Matrix** | `agentos-capability` | Per-resource rwx permissions with path-prefix matching and SSRF blocking |
| **3. Secrets Vault** | `agentos-vault` | AES-256-GCM encryption, Argon2id key derivation, `ZeroizingString` for in-memory secrets |
| **4. Syscall Sandbox** | `agentos-sandbox` | Seccomp-BPF filtering restricts which system calls tools can make (Linux-only) |
| **5. WASM Isolation** | `agentos-wasm` | Wasmtime sandbox for untrusted tool execution with controlled host access |
| **6. Tool Trust Tiers** | `agentos-tools` | Ed25519 signed manifests; 4 tiers: Core (trusted), Verified (signed), Community (signed), Blocked (rejected) |
| **7. Injection Scanning** | `agentos-kernel` | Prompt injection detection with risk classification; system prompt includes standing safety instructions |
| **8. API Authentication** | `agentos-api` | HMAC-SHA256 API keys with scope-based permissions, constant-time validation, optional expiry |

### Path traversal protection

All file tools reject any path containing `..` — this is a hard-coded security invariant enforced before capability token validation.

### Audit trail

Every security-relevant operation is logged to the append-only SQLite audit log (`agentos-audit`), which supports 85+ event types. The log cannot be modified or deleted through normal operation.

---

## Cost Tracking Architecture

The cost tracker enforces per-agent budgets and prevents runaway spending on LLM inference.

### Architecture

```
LLM Adapter (inference response)
    │
    ▼
CostTracker.record_inference()
    ├── Calculate cost from ModelPricing table (micro-USD precision)
    ├── Update AgentCostState (tokens, cost, tool calls)
    ├── Check against budget thresholds
    │   ├── < 80%  → Ok
    │   ├── 80-95% → Warning (broadcast BudgetAlert)
    │   ├── 95-100% → PauseRequired (broadcast BudgetAlert)
    │   └── > 100% → HardLimitExceeded (action: pause or kill task)
    └── Check model downgrade recommendation
        └── If configured, suggest cheaper model at threshold
```

### Budget enforcement

| Check | Scope | Description |
|-------|-------|-------------|
| **Token limit** | Per agent, daily | Maximum input + output tokens per 24h period |
| **Cost limit** | Per agent, daily | Maximum spend in USD per 24h period (tracked in micro-USD) |
| **Tool call limit** | Per agent, daily | Maximum tool executions per 24h period |
| **Wall time limit** | Per task | Maximum elapsed seconds for a single task |
| **Model allowlist** | Per agent | Restrict which models an agent can use |

### Budget thresholds

| Threshold | Default | Action |
|-----------|---------|--------|
| Warning | 80% | Broadcast `BudgetAlert`, continue execution |
| Pause | 95% | Broadcast `BudgetAlert`, pause task for human review |
| Hard limit | 100% | Stop task, emit `HardLimitExceeded` |

### Model downgrade

When an agent approaches its budget limit, the cost tracker can recommend a cheaper model:

```
Agent "gpt-agent" at 85% budget
  → ModelDowngradeRecommended { downgrade_to: "gpt-3.5-turbo", provider: "openai" }
```

### Pricing resolution

Model pricing is resolved in priority order:

1. **Exact match** — `openai/gpt-4` matches the pricing entry for `openai/gpt-4`
2. **Wildcard** — `ollama/*` matches any Ollama model
3. **Zero-cost fallback** — unknown models default to zero cost (logged as warning)

### Budget reset

Agent cost counters automatically reset on a 24-hour rolling boundary. The `period_start` timestamp tracks when the current budget period began.

### Cost attribution

Every inference generates a `CostAttribution` audit event with structured JSON containing the agent ID, model, provider, token counts, and calculated cost.
