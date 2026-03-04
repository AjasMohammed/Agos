# AgentOS — A Minimalist, LLM-Native Operating System

> *An agentic operating environment built in Rust, designed ground-up for LLMs and AI agents — not for humans.*

---

## Table of Contents

1. [Vision & Philosophy](#vision--philosophy)
2. [What Makes This Different](#what-makes-this-different)
3. [Core Concepts & Analogy to Linux](#core-concepts--analogy-to-linux)
4. [Architecture Overview](#architecture-overview)
5. [Layer 0 — The Inference Kernel](#layer-0--the-inference-kernel)
6. [Layer 1 — The Semantic IPC Bus](#layer-1--the-semantic-ipc-bus)
7. [Layer 2 — Agent Tools (OS Software)](#layer-2--agent-tools-os-software)
8. [Layer 3 — LLM Adapter Layer](#layer-3--llm-adapter-layer)
9. [Layer 4 — Intent Shell & UI](#layer-4--intent-shell--ui)
10. [Security Model](#security-model)
11. [Secrets & API Key Management](#secrets--api-key-management)
12. [Permission System — Linux-style for LLMs](#permission-system--linux-style-for-llms)
13. [Hardware Abstraction Layer](#hardware-abstraction-layer)
14. [GPU Support](#gpu-support)
15. [Background Tasks & Cron Jobs](#background-tasks--cron-jobs)
16. [Agent Registry & Awareness](#agent-registry--awareness)
17. [Agent-to-Agent Communication](#agent-to-agent-communication)
18. [Memory Architecture](#memory-architecture)
19. [Tool Ecosystem & SDK](#tool-ecosystem--sdk)
20. [Docker & Deployment](#docker--deployment)
21. [Comparison With Existing Work](#comparison-with-existing-work)
22. [Unsolved Challenges](#unsolved-challenges)
23. [Suggested Build Order](#suggested-build-order)
24. [Long-Term Vision](#long-term-vision)

---

## Vision & Philosophy

AgentOS is a purpose-built operating environment where **LLMs are the primary users**, not humans. Just as Linux was designed for human operators using terminals, AgentOS is designed for AI agents operating through structured semantic intent.

The core idea: instead of wrapping LLMs around existing operating systems (like most agent frameworks do), build a minimal environment from scratch where:

- **LLMs are the CPU** — they process, reason, and decide
- **Tools are the programs** — installed, versioned, and sandboxed
- **Intent is the syscall** — structured declarations replace raw function calls
- **The kernel manages everything** — scheduling, memory, security, context
- **Agents are peers** — every connected agent is aware of and can collaborate with others
- **Secrets are first-class** — API keys and credentials are encrypted at rest, never exposed to agents directly

AgentOS runs inside a Docker container, has no graphical interface for humans, exposes a CLI and optional Web UI for management, and allows multiple LLMs to be connected and routed simultaneously.

### Core Principles

- **Security is non-negotiable** — capability-based isolation, secrets vault, no feature trades security
- **Minimal by design** — every component exists for a reason; nothing more
- **LLM-native, not LLM-wrapped** — designed from first principles for agents
- **Multi-LLM by default** — connect OpenAI, Anthropic, Ollama, Gemini simultaneously
- **Agents are social** — every agent knows what other agents exist and can collaborate
- **Hardware is gated** — not all agents can touch hardware; access is explicitly granted per agent
- **Community extensible** — open tool SDK (Rust, Python, Node.js) so anyone can build tools

---

## What Makes This Different

Most existing agent frameworks (LangChain, CrewAI, LangGraph) are **libraries layered on top of existing operating systems**. MCP (Model Context Protocol) is a **communication protocol**, not an environment. AIOS runs **on top of** Linux. XKernel is still **partially layered over** traditional OS primitives. OpenFang is an excellent agent runtime but still runs as a binary on top of the host OS.

AgentOS is different in a fundamental way:

| Characteristic | Traditional OS | AgentOS |
|---|---|---|
| Primary user | Human | LLM / AI Agent |
| Interface | Terminal / GUI | Semantic Intent + CLI |
| Program format | ELF binary | Manifest + WASM/binary |
| IPC | Pipes, sockets, signals | Intent Channels (typed, async) |
| Syscall | Integer-keyed kernel call | Semantic Intent declaration |
| Scheduler | Process scheduler | Inference task scheduler |
| Memory | RAM pages | Context windows + vector store |
| Security | User permissions + ACLs | Capability tokens + sandboxing |
| Credentials | Env vars / config files | Encrypted secrets vault |
| Package manager | apt / pacman | Tool Registry |
| Runs inside | Bare metal / VM | Docker container |
| Agent awareness | N/A | Built-in agent registry + messaging bus |
| Hardware access | All processes | Per-agent, explicitly granted |

The key philosophical shift: **an LLM does not "execute" tools the way a human runs a program**. An LLM *declares intent*, and the kernel *decides* whether to honor it, which tool handles it, and how the result flows back into context. This separation is the foundation of the entire architecture.

---

## Core Concepts & Analogy to Linux

To understand AgentOS, map every concept directly to its Linux equivalent:

```
Linux                          AgentOS
─────────────────────────────────────────────────────────────────
Kernel                    →    Inference Kernel
Process                   →    Agent Task
System Call               →    Semantic Call (Intent)
Program / ELF Binary      →    Agent Tool (manifest + binary/WASM)
Shell (bash/zsh)          →    Intent Shell
IPC (pipes/sockets)       →    Intent Channels + Agent Message Bus
Filesystem                →    Semantic Store
User / Group Permissions  →    LLM Permission Matrix (rwx per resource)
Password / SSH Key        →    Secrets Vault (encrypted, kernel-managed)
Package Manager (apt)     →    Tool Registry
init / systemd            →    Task Supervisor (agentd)
cron                      →    Agent Scheduler
/proc virtual FS          →    Task Inspector API
cgroups                   →    Tool Resource Quotas
seccomp                   →    Tool Syscall Filter
/dev (device files)       →    Hardware Abstraction Layer (HAL)
GPU drivers               →    GPU Resource Manager
User directory listing    →    Agent Registry
Inter-process messaging   →    Agent-to-Agent Message Bus
```

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                          AgentOS Container                          │
│                                                                     │
│  ┌───────────────┐   ┌─────────────────┐   ┌─────────────────────┐  │
│  │ Intent Shell  │   │    Web UI        │   │  CLI / agentctl     │  │
│  │ (REPL / API)  │   │  (Axum / HTMX)  │   │                     │  │
│  └──────┬────────┘   └────────┬─────────┘   └──────────┬──────────┘  │
│         └────────────────────┴──────────────────────────┘            │
│                               │                                      │
│              ┌────────────────▼────────────────┐                     │
│              │          Inference Kernel        │                     │
│              │                                 │                     │
│              │  ┌──────────┬────────────────┐  │                     │
│              │  │Task Sched│  Cap Engine    │  │                     │
│              │  ├──────────┼────────────────┤  │                     │
│              │  │Ctx Mgr   │  Perm Matrix   │  │                     │
│              │  ├──────────┼────────────────┤  │                     │
│              │  │Mem Arb   │  GPU Manager   │  │                     │
│              │  ├──────────┼────────────────┤  │                     │
│              │  │Audit Log │  agentd / cron │  │                     │
│              │  ├──────────┼────────────────┤  │                     │
│              │  │Secrets   │  Agent Registry│  │                     │
│              │  │Vault     │  + Msg Bus     │  │                     │
│              │  └──────────┴────────────────┘  │                     │
│              └────────────────┬────────────────┘                     │
│                               │                                      │
│     ┌─────────────────────────┼──────────────────────────┐           │
│     │                         │                          │           │
│  ┌──▼──────────┐   ┌──────────▼──────┐   ┌──────────────▼────────┐  │
│  │ LLM Adapters│   │  Intent Bus     │   │  Hardware Abstr Layer │  │
│  │             │   │  (IPC)          │   │                       │  │
│  │ Ollama      │   └──────┬──────────┘   │  SensorDriver         │  │
│  │ OpenAI      │          │              │  StorageDriver        │  │
│  │ Anthropic   │   ┌──────▼──────────┐   │  NetworkDriver        │  │
│  │ Gemini      │   │  Tool Registry  │   │  GPUDriver            │  │
│  │ Custom      │   │                 │   │  ProcessDriver        │  │
│  └─────────────┘   │  Rust / WASM    │   └───────────────────────┘  │
│                    │  Python tools   │                               │
│                    │  Node tools     │                               │
│                    └─────────────────┘                               │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Layer 0 — The Inference Kernel

The kernel is the core of AgentOS. Written entirely in Rust, it manages the lifecycle of every agent task and mediates all resource access.

### Components

#### Task Scheduler
- Maintains a priority queue of in-flight LLM inference tasks
- Handles async task execution — LLM calls are expensive (100ms–10s), never block the kernel
- Supports task preemption, timeout enforcement, and retry logic
- Task states: `Queued → Running → Waiting → Complete → Failed`

```rust
pub struct AgentTask {
    pub id: TaskID,
    pub state: TaskState,
    pub context: ContextWindow,
    pub capability_token: CapabilityToken,
    pub assigned_llm: Option<LLMCoreID>,
    pub priority: u8,
    pub created_at: Instant,
    pub timeout: Duration,
    pub history: Vec<IntentMessage>,
    pub parent_task: Option<TaskID>,      // for sub-agent delegation
    pub agent_id: AgentID,                // which agent owns this task
}
```

#### Context Manager
- Maintains rolling context windows per task
- Handles context overflow via summarization or eviction strategies
- Tracks which tools have been called and what data has flowed through
- Supports context checkpointing for task rollback
- Injects agent-awareness context (agent directory) at task start

#### Capability Engine
- Issues unforgeable, scoped capability tokens for every task
- Tokens encode: task ID, agent ID, allowed tool IDs, allowed intent types, hardware access flags, expiry
- All tool invocations are checked against the issuing task's token
- Tokens cannot be forged, copied, or escalated — enforced at the type level in Rust

```rust
pub struct CapabilityToken {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub allowed_tools: BTreeSet<ToolID>,
    pub allowed_intents: EnumSet<IntentType>,
    pub hardware_access: HardwarePermissions,   // derived from agent's perm matrix
    pub issued_at: Instant,
    pub expires_at: Instant,
    pub signature: HmacSha256Signature,          // kernel-signed, unforgeable
}
```

#### Memory Arbiter
Manages three memory tiers per task — see [Memory Architecture](#memory-architecture).

#### Secrets Vault
Encrypted credential store — see [Secrets & API Key Management](#secrets--api-key-management).

#### Agent Registry
Built-in directory of all connected agents — see [Agent Registry & Awareness](#agent-registry--awareness).

#### GPU Manager
Tracks GPU devices and allocates VRAM per tool execution — see [GPU Support](#gpu-support).

---

## Layer 1 — The Semantic IPC Bus

This is the communication layer that replaces MCP, function-calling, and raw JSON tool calls. The key design insight: **LLMs communicate with structured semantic intent, not pointers and integers**.

### Intent Message Format

Every communication between the LLM, kernel, and tools uses this envelope:

```rust
pub struct IntentMessage {
    pub id: MessageID,
    pub sender_token: CapabilityToken,   // Who is sending
    pub intent_type: IntentType,         // What kind of action
    pub target: IntentTarget,            // Tool, hardware resource, or another agent
    pub payload: SemanticPayload,        // Validated, schema-checked data
    pub context_ref: ContextID,          // Which task this belongs to
    pub priority: u8,
    pub timeout_ms: u32,
    pub trace_id: TraceID,               // For audit logging
}

pub enum IntentTarget {
    Tool(ToolID),
    Hardware(HardwareResource),
    Agent(AgentID),                      // direct agent-to-agent messaging
}

pub enum IntentType {
    Read,       // Read data from a resource
    Write,      // Write or modify a resource
    Execute,    // Run a computation
    Query,      // Search or retrieve information
    Observe,    // Monitor or watch a resource
    Delegate,   // Spawn a sub-agent task
    Message,    // Send a message to another agent
    Broadcast,  // Broadcast to all agents in a group
}
```

### Transport

Intent Channels use **Unix domain sockets** internally — no HTTP overhead, no network stack round-trips. For WASM tools, shared-memory channels are used. Communication overhead is measured in microseconds, not milliseconds.

### Why Not MCP or Function Calling?

| Aspect | Function Calling / MCP | Intent Bus |
|---|---|---|
| Type safety | JSON schema (runtime) | Rust types (compile time) |
| Security check | Application level | Kernel level (unforgeable tokens) |
| Auditability | Optional | Mandatory, every message logged |
| Routing | Manual (developer picks tool) | Semantic matching (kernel routes) |
| Agent targeting | Not supported | Native (IntentTarget::Agent) |
| Transport | HTTP / stdio | Unix domain sockets (near-zero overhead) |
| Sandboxing | None | Per-message capability check |

---

## Layer 2 — Agent Tools (OS Software)

Tools are the "programs" of AgentOS. Unlike traditional software designed for human interaction, Agent Tools are designed entirely for LLM consumption. They have no UI. They have a **machine-readable manifest** and a **typed interface**.

### Tool Manifest Format

```toml
[manifest]
name        = "file-reader"
version     = "1.2.0"
description = "Reads files from the semantic store and returns structured content"
author      = "community"
checksum    = "sha256:abc123..."

[capabilities_required]
permissions = ["fs.read", "context.write"]

[capabilities_provided]
outputs = ["content.text", "content.structured", "content.binary"]

[intent_schema]
input  = "FileReadIntent"
output = "FileContent"

[sandbox]
network       = false
fs_write      = false
gpu           = false
hardware      = []                     # no hardware access
max_memory_mb = 64
max_cpu_ms    = 5000
syscalls      = ["read", "write", "mmap"]
```

### Tool Execution Flow

```
LLM emits intent: "I need to read file /data/report.pdf"
          │
          ▼
Kernel receives IntentMessage { intent_type: Read, target: Tool("file-reader"), ... }
          │
          ▼
Capability check: does this agent's permission matrix allow fs.read?
          │
          ▼
Schema validation: does payload match FileReadIntent schema?
          │
          ▼
Tool spawned in isolated process / WASM sandbox with seccomp profile applied
          │
          ▼
Result returned via Intent Channel
          │
          ▼
Output sanitized — wrapped in delimiters before context injection
          │
          ▼
LLM receives: [TOOL_RESULT: file-reader] { content: "..." } [/TOOL_RESULT]
```

### Standard Library Tools (Built-in)

| Tool | Description |
|---|---|
| `file-reader` | Read files from the semantic store |
| `file-writer` | Write files with capability-gated access |
| `http-client` | Outbound HTTP requests |
| `code-runner` | Execute code snippets in isolation |
| `memory-search` | Query semantic memory vector store |
| `memory-write` | Write to long-term semantic memory |
| `task-delegate` | Spawn a sub-agent task |
| `data-parser` | Parse JSON, CSV, XML, Markdown, PDF |
| `shell-exec` | Shell commands (highly restricted capability) |
| `sys-monitor` | Read CPU, RAM, disk usage |
| `process-manager` | List and kill processes (requires permission) |
| `log-reader` | Read app / network / system logs |
| `network-monitor` | Read network interface stats |
| `hardware-info` | GPU, CPU, device enumeration |
| `agent-message` | Send a message to another agent |

---

## Layer 3 — LLM Adapter Layer

Every connected LLM is treated as a **heterogeneous compute core**. The kernel schedules tasks to them based on capability, availability, and cost.

### LLMCore Trait (Rust)

```rust
#[async_trait]
pub trait LLMCore: Send + Sync {
    async fn infer(
        &self,
        context: ContextWindow,
        intent: Intent,
    ) -> Result<InferenceResult, LLMError>;

    fn capabilities(&self) -> ModelCapabilities;
    fn cost_estimate(&self, input_tokens: usize, output_tokens: usize) -> f64;
    fn latency_profile(&self) -> LatencyProfile;
    async fn health_check(&self) -> bool;
    fn agent_id(&self) -> AgentID;   // every LLM has a unique agent identity
}
```

### Supported Adapters

| Adapter | Backend | Notes |
|---|---|---|
| `OllamaCore` | Local Ollama | Best for offline/dev use |
| `OpenAICore` | OpenAI API | GPT-4o, o1, etc. |
| `AnthropicCore` | Anthropic API | Claude 3.5+, etc. |
| `GeminiCore` | Google AI | Gemini 1.5 Pro, etc. |
| `CustomCore` | Any OpenAI-compatible API | Local llama.cpp, vLLM, etc. |

### Task Routing Strategy

```toml
[routing]
strategy = "capability-first"  # or "cost-first", "latency-first", "round-robin"

[[routing.rules]]
task_type     = "code-execution"
preferred_llm = "anthropic/claude-sonnet"
fallback_llm  = "ollama/llama3.2"

[[routing.rules]]
task_type     = "quick-summary"
preferred_llm = "ollama/llama3.2"
```

---

## Layer 4 — Intent Shell & UI

### CLI (`agentctl`)

```bash
# Connect an LLM — API key stored securely in vault, never as CLI arg
agentctl agent connect --provider anthropic --model claude-sonnet-4 --name "analyst"
# Prompts interactively: "Enter API key (input hidden):"

# Grant permissions to an agent (Linux-style)
agentctl perm grant analyst network.logs:r
agentctl perm grant analyst process.list:r
agentctl perm grant analyst hardware.sensors:r

# Run a task
agentctl task run --agent analyst "Summarize error logs from the last 24 hours"

# Agent-to-agent
agentctl agent list
agentctl agent message analyst summarizer "Here is your input data: ..."

# Background / cron
agentctl schedule create --name "daily-report" --cron "0 8 * * *" --agent analyst \
  --task "Compile daily system health report"

# Tool management
agentctl tool install web-search
agentctl tool list

# Secrets
agentctl secret set SLACK_TOKEN
agentctl secret list
agentctl secret revoke SLACK_TOKEN

# System
agentctl status
agentctl audit logs --last 100
```

### Web UI

A minimal management dashboard (Axum backend + HTMX frontend):

- **Dashboard**: Active tasks, connected agents, tool registry, system health
- **Agent Manager**: Connect, configure, set permissions, view agent profiles
- **Task Inspector**: Real-time intent stream, context window viewer, tool call timeline
- **Tool Manager**: Browse, install, remove, and inspect tools
- **Secrets Manager**: Add, view (metadata only), and revoke credentials
- **Audit Log**: Searchable, filterable log of every intent message and tool execution
- **Agent Communication View**: Visual graph of agent interactions and message history

---

## Security Model

Security is a first-class citizen. The threat model assumes prompt injection, tool privilege escalation, rogue tasks, and supply chain attacks.

### Defense in Depth

**1. Capability-Based Access Control** — every resource access requires an unforgeable, scoped, kernel-signed token. Tokens expire with the task.

**2. Tool Sandboxing** — every tool runs under hard seccomp constraints derived from its manifest. Hardware access requires explicit declaration and agent permission.

**3. Intent Verification** — the kernel validates every intent against the task's capability token *before* execution. Hardware intents get an additional HAL permission check.

**4. Output Sanitization** — tool outputs are never injected raw into LLM context. They are wrapped in typed delimiters and treated as untrusted data.

**5. Immutable Audit Log** — every intent message, tool execution, agent message, and LLM call is written to an append-only log. Only the kernel can write to it.

**6. Secrets Isolation** — API keys and credentials are never placed in environment variables or config files. They live in the encrypted vault and are retrieved only by the kernel at LLM adapter initialization. No tool or agent ever sees a raw credential.

**7. Tool Registry Trust** — installed tools must have a valid SHA-256 checksum and signed manifest. Community tools are marked unverified until reviewed.

**8. Agent Identity Signing** — every agent message is signed with the sender's kernel-issued identity token. Agents cannot impersonate one another.

---

## Secrets & API Key Management

This is a first-class kernel subsystem. When users connect LLMs that require API keys, those keys are never stored in plaintext environment variables, config files, or docker-compose YAML.

### How It Works

```
User runs: agentctl agent connect --provider openai --model gpt-4o --name "researcher"
          │
          ▼
agentctl prompts interactively: "Enter API key (input hidden):"
          │
          ▼
Key passed over local Unix socket to the Secrets Vault
          │
          ▼
Vault encrypts key with AES-256-GCM using a machine-derived master key
(Master key is never stored as plaintext)
          │
          ▼
Encrypted blob stored in vault DB (SQLite with SQLCipher encryption)
          │
          ▼
At runtime: kernel retrieves and decrypts key in memory
Key is zeroed from memory after use (Rust zeroize crate)
No tool, no agent, no user CLI command can read the raw key back
```

### Secrets Vault Architecture

```rust
pub struct SecretsVault {
    db: EncryptedSqlite,          // SQLCipher-backed, AES-256
    master_key: ZeroizingKey,     // Zeroized from memory when not in use
}

pub struct SecretEntry {
    pub id: SecretID,
    pub name: String,              // e.g. "OPENAI_API_KEY"
    pub owner: SecretOwner,        // Kernel | Agent(AgentID) | Tool(ToolID)
    pub scope: SecretScope,        // who can request this secret
    pub created_at: DateTime,
    pub last_used_at: DateTime,
    pub encrypted_value: Vec<u8>,  // AES-256-GCM ciphertext — never raw
    pub access_log: Vec<SecretAccess>,
}
```

### CLI Interface

```bash
# Add a secret (interactive — never appears in shell history)
agentctl secret set OPENAI_API_KEY

# Scoped to a specific agent only
agentctl secret set SLACK_TOKEN --scope agent:notifier

# Scoped to a specific tool only
agentctl secret set DB_PASSWORD --scope tool:database-query

# List secrets (names and metadata only — values never shown)
agentctl secret list
# NAME               SCOPE           LAST USED
# OPENAI_API_KEY     global          2 mins ago
# SLACK_TOKEN        agent:notifier  1 hour ago
# DB_PASSWORD        tool:db-query   never

# Revoke a secret
agentctl secret revoke SLACK_TOKEN

# Atomic rotation (new key replaces old without downtime)
agentctl secret rotate OPENAI_API_KEY
```

### What Never Happens

- API keys are **never** passed as CLI arguments (shell history exposure)
- API keys are **never** stored in `docker-compose.yml` or `.env` files
- API keys are **never** visible to any tool, agent, or web UI
- Secrets are **zeroed from memory** immediately after use

---

## Permission System — Linux-style for LLMs

Every connected agent has a permission matrix controlling exactly which OS resources it can access. Directly modelled on Unix `rwx` permissions but extended for AI-native resource classes.

### Permission Bit Model

```
Resource Class          r (read)          w (write)         x (execute/act)
────────────────────────────────────────────────────────────────────────────
network.logs            read logs         —                 —
network.outbound        —                 —                 make HTTP calls
process.list            list processes    —                 —
process.kill            —                 —                 kill processes
fs.app_logs             read app logs     —                 —
fs.system_logs          read system logs  —                 —
fs.user_data            read files        write files       —
hardware.sensors        read values       —                 —
hardware.gpio           read pin state    set pin state     trigger
hardware.gpu            query info        —                 use for compute
cron.jobs               view scheduled    create new job    delete / run
memory.semantic         read              write             —
memory.episodic         read              —                 —
agent.message           receive msgs      —                 send msgs
agent.broadcast         receive           —                 broadcast
```

### `agentperm` CLI

```bash
# Grant a single permission
agentctl perm grant analyst network.logs:r

# Grant multiple at once
agentctl perm grant analyst process.list:r,fs.app_logs:r,network.logs:r

# Grant hardware access (off by default for ALL agents)
agentctl perm grant analyst hardware.sensors:r
agentctl perm grant researcher hardware.gpu:rx

# Revoke a permission
agentctl perm revoke analyst process.kill:x

# View all permissions for an agent (like ls -la)
agentctl perm show analyst
# RESOURCE              R    W    X    SCOPE
# network.logs          ✓    -    -    all
# process.list          ✓    -    -    all
# process.kill          -    -    -    (denied)
# hardware.sensors      ✓    -    -    all
# hardware.gpu          -    -    -    (denied)

# Create a reusable permission profile (like a Unix group)
agentctl perm profile create "ops-agent" \
  --grant "network.logs:r,process.list:r,fs.app_logs:r"
agentctl perm profile assign analyst ops-agent

# Time-limited permission (auto-expires)
agentctl perm grant analyst fs.system_logs:r --expires 2h
```

### Kernel Enforcement

Permissions are enforced in the kernel **before** any Intent Message reaches a tool:

```
Agent emits intent
    → Kernel checks CapabilityToken
    → Checks agent's PermissionMatrix
    → If denied: returns PermissionDenied, logs to audit
    → If allowed: forwards to tool / HAL with scoped token
```

Permission checks happen at the Rust type level — there is no code path to bypass them.

### Hardware Permissions Are Off By Default

No agent can access any hardware resource unless explicitly granted. This includes sensors, GPIO, GPU compute, storage devices, and network interfaces. Hardware access is **opt-in, not opt-out** — the inverse of traditional OS design.

---

## Hardware Abstraction Layer

The HAL sits between the kernel and physical hardware. LLMs never receive raw device access (`/dev/ttyUSB0`). They interact with typed hardware abstractions that the kernel mediates.

### HAL Architecture

```
Agent Intent (requires hardware.sensors:r permission)
          │
          ▼
Kernel: HAL permission check against agent's permission matrix
          │
          ▼
HAL Interface (abstract Rust trait)
          │
          ├──→ SensorDriver     (temperature, humidity, GPS, accelerometer)
          ├──→ StorageDriver    (NVMe, USB, SD card — beyond semantic store)
          ├──→ NetworkDriver    (interface stats, packet capture — read only)
          ├──→ ProcessDriver    (process list, resource usage, kill)
          ├──→ GPIODriver       (embedded / IoT pin control)
          ├──→ GPUDriver        (VRAM query, compute allocation)
          └──→ SystemDriver     (CPU stats, memory, uptime, kernel info)
```

### Hardware Tool Manifest

```toml
[manifest]
name          = "temperature-sensor"
version       = "1.0"
description   = "Read temperature from connected sensor array"
hardware_type = "sensor"

[capabilities_required]
permissions = ["hardware.sensors:r"]

[sandbox]
network       = false
fs_write      = false
hardware      = ["sensor.temperature", "sensor.humidity"]
max_memory_mb = 16
```

### System Inspection Tools (HAL-backed)

| Tool | Permission Required | What It Does |
|---|---|---|
| `sys-monitor` | `hardware.system:r` | CPU, RAM, disk, uptime |
| `process-manager` | `process.list:r` + `process.kill:x` | List/kill processes |
| `log-reader` | `fs.app_logs:r` or `fs.system_logs:r` | Read structured logs |
| `network-monitor` | `network.logs:r` | Interface stats, connection table |
| `hardware-info` | `hardware.system:r` | GPU, CPU, device enumeration |
| `gpio-control` | `hardware.gpio:rw` | Read/write GPIO pins |
| `sensor-reader` | `hardware.sensors:r` | Read connected sensors |

---

## GPU Support

### Two Use Cases

**Use Case A — Local LLM inference acceleration:** When running local models via Ollama or a Rust-native inference engine, the kernel detects available GPUs and routes inference requests through the GPU Manager for hardware acceleration.

**Use Case B — Tool-level GPU compute:** Tools that declare `gpu = true` in their manifest receive a VRAM allocation from the GPU Manager for the duration of their execution (embeddings, computer vision, data processing, etc.).

### GPU Resource Manager

```rust
pub struct GPUResourceManager {
    pub devices: Vec<GPUDevice>,
    pub allocations: HashMap<TaskID, GPUAllocation>,
}

pub struct GPUDevice {
    pub device_id: u32,
    pub vram_total_mb: u64,
    pub vram_available_mb: u64,
    pub backend: GPUBackend,
    pub utilization_percent: f32,
}

pub enum GPUBackend {
    CUDA,     // NVIDIA
    Metal,    // Apple Silicon
    Vulkan,   // Cross-platform
    ROCm,     // AMD
    WebGPU,   // Fallback
}
```

### GPU Permission Gate

GPU access requires explicit permission in the agent's permission matrix:

```bash
# Grant GPU compute access to a specific agent only
agentctl perm grant researcher hardware.gpu:rx

# Without this, tool manifests declaring gpu=true will be denied at the kernel level
```

### Tool Manifest with GPU

```toml
[sandbox]
network       = false
gpu           = true
gpu_vram_mb   = 512        # VRAM quota for this tool execution
gpu_backend   = "auto"     # kernel picks best available backend
```

### Docker GPU Passthrough

```yaml
services:
  agentos:
    image: agentos:latest
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all
              capabilities: [gpu]
    environment:
      - NVIDIA_VISIBLE_DEVICES=all
```

---

## Background Tasks & Cron Jobs

### The `agentd` Supervisor

`agentd` is the AgentOS equivalent of `systemd` + `cron` + `supervisord`. It manages long-running and scheduled agent tasks independently of interactive sessions.

### Scheduling CLI

```bash
# Create a recurring scheduled task
agentctl schedule create \
  --name "daily-log-summary" \
  --cron "0 8 * * *" \
  --agent analyst \
  --task "Summarize all application error logs from the last 24 hours" \
  --permissions "fs.app_logs:r,fs.user_data:w"

# One-shot background task (detached)
agentctl bg run \
  --name "process-uploads" \
  --agent researcher \
  --task "Process all files in /data/incoming and classify by type" \
  --detach

# List scheduled jobs (like crontab -l)
agentctl schedule list

# List running background tasks (like ps aux)
agentctl bg list

# Follow logs (like journalctl -f)
agentctl bg logs daily-log-summary --follow

# Kill a background task
agentctl bg kill process-uploads

# Pause / resume a scheduled job
agentctl schedule pause daily-log-summary
agentctl schedule resume daily-log-summary
```

### Background Task Properties

Every background task has:
- Its own isolated context window (no bleed between tasks)
- A capability token scoped to permissions declared at schedule time
- Resource quota: max LLM tokens per run, max wall-clock time, max memory
- Output destination: where results are written on completion
- Retry policy: configurable backoff on failure
- Notification hook: optional agent message on completion or failure

### Task Lifecycle

```
Scheduled ──→ Queued ──→ Running ──→ Complete
                │            │
                │            ├──→ Waiting (on tool / sub-agent)
                │            └──→ Failed ──→ RetryQueue
                └──→ Paused
```

---

## Agent Registry & Awareness

Every connected LLM is registered as a named **Agent** in the Agent Registry — a kernel-managed directory analogous to `/etc/passwd` in Linux. Every agent knows that other agents exist and can query their capabilities.

### Agent Profile

```rust
pub struct AgentProfile {
    pub id: AgentID,
    pub name: String,                      // e.g. "analyst", "researcher"
    pub provider: LLMProvider,             // Anthropic, OpenAI, Ollama, etc.
    pub model: String,                     // e.g. "claude-sonnet-4"
    pub capabilities: ModelCapabilities,   // context size, modalities, etc.
    pub permissions: LLMPermissions,       // the agent's permission matrix
    pub status: AgentStatus,               // Online | Idle | Busy | Offline
    pub current_task: Option<TaskID>,
    pub description: String,               // human-readable role description
    pub tags: Vec<String>,                 // e.g. ["analysis", "code", "vision"]
    pub created_at: DateTime,
    pub last_active: DateTime,
    pub message_bus_address: BusAddress,
}
```

### Agent Awareness in Context

When an agent starts a task, the kernel automatically injects a compact **Agent Directory** into the context window:

```
[AGENT_DIRECTORY]
You are operating inside AgentOS. The following agents are available for collaboration:

- analyst (Anthropic/claude-sonnet-4) — Status: Idle
  Role: Data analysis and log summarization
  Can receive: messages, delegated tasks

- researcher (OpenAI/gpt-4o) — Status: Busy (task-042)
  Role: Web research and information gathering
  Can receive: messages

- summarizer (Ollama/llama3.2) — Status: Idle
  Role: Fast text summarization and classification
  Can receive: messages, delegated tasks

To send a message to an agent: use the agent-message tool
To delegate a subtask: use the task-delegate tool with agent_id
[/AGENT_DIRECTORY]
```

Every agent always has situational awareness — who is available, what they do, and whether they are currently busy.

---

## Agent-to-Agent Communication

Agents communicate and collaborate through the **Agent Message Bus**, a kernel-managed pub/sub + direct messaging system.

### Communication Modes

**1. Direct Message** — one agent sends a structured message to a specific agent

**2. Task Delegation** — an agent hands off a subtask to another agent and waits for the result

**3. Broadcast** — an agent sends a message to all agents in a group or all active agents

**4. Collaborative Pipeline** — the user defines a multi-agent pipeline where outputs are automatically routed between agents in sequence

### Task Delegation Flow

```
analyst receives task: "Process 10,000 log files and generate a trend report"
          │
analyst decides: "summarizer is better suited for bulk text processing"
          │
          ▼
analyst uses task-delegate tool:
  { target_agent: "summarizer", task: "Summarize these logs...", priority: 5 }
          │
          ▼
Kernel creates a child task owned by summarizer
Child task receives a RESTRICTED capability token
(parent cannot grant the child more permissions than the parent has)
          │
          ▼
summarizer executes, returns result via Intent Channel
          │
          ▼
Result injected back into analyst's context — analyst continues
```

### Multi-Agent Pipeline

Users can define reusable pipelines where data flows automatically between agents:

```yaml
# pipeline: research-and-report.yaml
name: "Research and Report Pipeline"
steps:
  - agent: researcher
    task: "Search the web for: {input}"
    output: raw_research

  - agent: analyst
    task: "Analyze this research and extract key findings: {raw_research}"
    output: analysis

  - agent: summarizer
    task: "Write an executive summary based on: {analysis}"
    output: final_report

  - tool: file-writer
    input: final_report
    destination: "/output/report-{date}.md"
```

```bash
# Run a pipeline
agentctl pipeline run research-and-report.yaml \
  --input "latest developments in Rust async runtimes"

# List pipelines
agentctl pipeline list

# View execution status
agentctl pipeline status research-and-report --run-id abc123
```

### Agent Message Bus Architecture

```rust
pub struct AgentMessageBus {
    channels: HashMap<AgentID, UnboundedSender<AgentMessage>>,
    groups: HashMap<GroupID, Vec<AgentID>>,
    history: AppendOnlyLog<AgentMessage>,   // all messages audited
}

pub struct AgentMessage {
    pub id: MessageID,
    pub from: AgentID,
    pub to: MessageTarget,       // Direct(AgentID) | Group(GroupID) | Broadcast
    pub content: MessageContent,
    pub reply_to: Option<MessageID>,
    pub timestamp: DateTime,
    pub trace_id: TraceID,
}
```

### Security in Agent Communication

- Sending requires `agent.message:x` permission; receiving requires `agent.message:r`
- Delegated tasks receive strictly downscoped tokens — a child task can never have more permissions than its parent
- All agent messages are logged to the immutable audit log
- Agents cannot impersonate each other — messages are signed with kernel-issued identity tokens
- The user can fully disable agent communication for any agent: `agentctl perm revoke analyst agent.message:rx`

---

## Memory Architecture

```
┌─────────────────────────────────────────────────────────┐
│                   Memory Architecture                    │
│                                                         │
│  Tier 1: Working Memory (per-task, in-memory)           │
│  ┌─────────────────────────────────────────────────┐    │
│  │  Active context window — ring buffer            │    │
│  │  Includes: agent directory, task history,       │    │
│  │  tool results, received agent messages          │    │
│  │  Overflow: oldest entries summarized/evicted    │    │
│  └─────────────────────────────────────────────────┘    │
│                           │                             │
│  Tier 2: Episodic Memory (per-task, persisted)          │
│  ┌─────────────────────────────────────────────────┐    │
│  │  Full task history — SQLite per task            │    │
│  │  Intent messages, tool calls, agent messages,   │    │
│  │  LLM responses — indexed for recall             │    │
│  └─────────────────────────────────────────────────┘    │
│                           │                             │
│  Tier 3: Semantic Memory (global, persisted)            │
│  ┌─────────────────────────────────────────────────┐    │
│  │  Long-term recall — embedded vector store       │    │
│  │  Cross-task, cross-agent, cross-session         │    │
│  │  Accessed via memory-search / memory-write      │    │
│  │  Permission-gated: agents can have different    │    │
│  │  read/write scopes over shared memory           │    │
│  └─────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
```

Memory access is always mediated by the kernel — agents never have direct pointer access to any memory tier. Agents can be granted or denied access to other agents' episodic memory by the user.

---

## Tool Ecosystem & SDK

### Rust SDK

```rust
use agentos_sdk::{tool, ToolContext, ToolResult};

#[tool(
    name = "web-search",
    version = "1.0.0",
    description = "Searches the web and returns structured results",
    capabilities_required = ["network.outbound"],
    sandbox_network = true,
    max_memory_mb = 128,
)]
async fn web_search(ctx: ToolContext, intent: WebSearchIntent) -> ToolResult<SearchResults> {
    let results = ctx.http_client().get(&format!("https://...{}", intent.query)).await?;
    Ok(SearchResults { items: parse_results(results) })
}
```

### Python SDK

```python
from agentos import tool, ToolContext

@tool(
    name="database-query",
    version="1.0.0",
    description="Execute read-only SQL queries",
    capabilities_required=["db.read"],
    sandbox={"network": False, "max_memory_mb": 128}
)
async def database_query(ctx: ToolContext, intent: dict) -> dict:
    query = intent["query"]
    db_pass = ctx.secrets.get("DB_PASSWORD")  # from vault, never hardcoded
    db = await connect(password=db_pass)
    result = await db.execute_readonly(query)
    return {"rows": result.to_dict(), "schema": result.schema}
```

```bash
pip install agentos-sdk
agentos-cli package --entry database_query.py
agentos-cli install ./database-query-1.0.0.aot
```

### Node.js / TypeScript SDK

```typescript
import { tool, ToolContext } from '@agentos/sdk';

export default tool({
  name: 'slack-notifier',
  version: '1.0.0',
  description: 'Send notifications to Slack channels',
  capabilitiesRequired: ['network.outbound'],
  sandbox: { network: true, maxMemoryMb: 64 }
}, async (ctx: ToolContext, intent: SlackIntent) => {
  const token = ctx.secrets.get('SLACK_TOKEN');  // from vault
  await ctx.http.post('https://slack.com/api/chat.postMessage', {
    channel: intent.channel,
    text: intent.message,
    headers: { Authorization: `Bearer ${token}` }
  });
  return { success: true };
});
```

```bash
npm install @agentos/sdk
npx agentos-cli package
npx agentos-cli install ./slack-notifier-1.0.0.aot
```

### Tool Registry Trust Tiers

| Tier | Description |
|---|---|
| **Core** | Ships with AgentOS, fully audited |
| **Verified** | Community tools that passed formal review |
| **Community** | Published but unreviewed (installed with warning) |
| **Local** | Private tools, not published |

---

## Docker & Deployment

### Dockerfile

```dockerfile
FROM rust:alpine AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin agentos-kernel

FROM alpine:latest
RUN apk add --no-cache wasmtime sqlcipher
COPY --from=builder /build/target/release/agentos-kernel /usr/bin/
COPY --from=builder /build/tools/core/ /opt/agentos/tools/core/

VOLUME ["/opt/agentos/data", "/opt/agentos/tools/user", "/opt/agentos/vault"]
EXPOSE 8080   # Web UI
EXPOSE 9090   # Kernel API (internal)

ENTRYPOINT ["/usr/bin/agentos-kernel"]
```

### docker-compose.yml

```yaml
version: "3.9"
services:
  agentos:
    image: agentos:latest
    ports:
      - "8080:8080"
    volumes:
      - ./data:/opt/agentos/data
      - ./tools:/opt/agentos/tools/user
      - ./vault:/opt/agentos/vault       # encrypted secrets vault
    # NOTE: No API keys in environment variables
    # Use: agentctl secret set OPENAI_API_KEY
    depends_on:
      - ollama
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all
              capabilities: [gpu]        # optional GPU passthrough

  ollama:
    image: ollama/ollama:latest
    volumes:
      - ollama_data:/root/.ollama

volumes:
  ollama_data:
```

### Target Image Size

| Component | Size |
|---|---|
| Base alpine | ~5 MB |
| Rust kernel binary | ~15 MB |
| Wasmtime runtime | ~20 MB |
| Core tools | ~10 MB |
| SQLCipher (vault) | ~3 MB |
| **Total target** | **< 60 MB** |

---

## Comparison With Existing Work

| Feature | AIOS | XKernel | OpenFang | LangGraph | **AgentOS** |
|---|---|---|---|---|---|
| Built in Rust | Partial | Yes | Yes | No | **Yes** |
| Runs in Docker | Yes | No | Yes | Yes | **Yes** |
| Multi-LLM routing | Yes | Partial | Yes (27) | Yes | **Yes** |
| Purpose-built (not layered on Linux) | No | Partial | No | No | **Yes** |
| Capability-based security | No | Partial | No | No | **Yes** |
| Encrypted secrets vault | No | No | No | No | **Yes** |
| Linux-style permission matrix per agent | No | No | No | No | **Yes** |
| Hardware access is gated per agent | No | No | No | No | **Yes** |
| Hardware abstraction layer | No | No | No | No | **Yes** |
| GPU resource management | No | No | No | No | **Yes** |
| Background tasks / cron | No | Partial | Yes | Partial | **Yes** |
| Agent-to-agent messaging | No | No | No | Yes | **Yes** |
| Multi-agent pipelines | No | No | No | Yes | **Yes** |
| Agent registry + awareness | No | No | No | No | **Yes** |
| Python + Node.js SDKs | No | No | No | Yes | **Yes** |
| Semantic IPC (not HTTP/JSON) | No | No | No | No | **Yes** |
| Immutable audit log | No | No | Partial | No | **Yes** |
| Tool sandboxing (seccomp + WASM) | No | Partial | WASM only | No | **Yes** |
| Open tool ecosystem | No | No | No | Yes | **Yes** |

---

## Unsolved Challenges

### 1. Semantic Syscall Specification
A stable ABI for how diverse LLMs express intent — different models prompt differently. Must be expressive enough for real-world tasks yet stable enough to be the kernel's foundation. Likely approach: a fixed intent type enum + schema-validated structured payload.

### 2. Context Statefulness
LLMs are stateless but tasks are not. The kernel needs checkpoint/rollback support, per-task state machines, and graceful handling of context window exhaustion mid-task.

### 3. Scheduler Performance Under Load
LLM inference is slow and unpredictable. The scheduler needs parallel task execution, graceful degradation when LLMs are unavailable, and fair queuing — without corrupting task state.

### 4. Agent Coordination Deadlocks
As multi-agent pipelines grow complex, deadlocks (agent A waiting on B, B waiting on A) and circular delegation become real risks. A formal concurrency model with deadlock detection is needed.

### 5. Prompt Injection at Scale
Every new tool is a potential injection vector. Continuous red-team testing and possibly a dedicated safety-filter kernel module running before every context injection.

### 6. Tool Ecosystem Cold Start
Need a standard library of 20+ tools that make AgentOS genuinely useful on day one.

### 7. Secrets Vault Master Key Bootstrap
How is the master encryption key derived and protected in headless / automated deployments? Options: HSM integration, TPM-backed keys, or user-supplied passphrase with KDF.

### 8. Agent Identity Across Restarts
When the container restarts, agents must re-establish identity without re-entering credentials. Need a persistent agent identity model backed by the secrets vault.

---

## Suggested Build Order

### Phase 1 — Foundation (Weeks 1–4)
- [ ] Define Intent Message schema and IntentTarget enum
- [ ] Implement CapabilityToken system with HMAC signing
- [ ] Build minimal Inference Kernel: task queue, context manager, Intent Channel
- [ ] Implement Secrets Vault (SQLCipher + AES-256-GCM, zeroize)
- [ ] Implement OllamaCore LLM adapter
- [ ] Basic CLI: `agent connect`, `task run`, `task logs`, `secret set`

### Phase 2 — Tools & Permissions (Weeks 5–8)
- [ ] Define tool manifest format and validation
- [ ] Build tool loader with sandboxed process spawning and seccomp
- [ ] Implement 8 core tools: `file-reader/writer`, `memory-search/write`, `data-parser`, `http-client`, `sys-monitor`, `log-reader`
- [ ] Implement LLM Permission Matrix and `agentperm` CLI
- [ ] CLI: `tool install/list/remove`, `perm grant/revoke/show`

### Phase 3 — Multi-LLM, HAL & GPU (Weeks 9–12)
- [ ] Implement OpenAI and Anthropic adapters
- [ ] Build Hardware Abstraction Layer (SensorDriver, ProcessDriver, SystemDriver)
- [ ] GPU Resource Manager with CUDA/Metal/Vulkan detection
- [ ] Task routing scheduler with configurable policy
- [ ] Three-tier memory architecture
- [ ] Immutable audit log

### Phase 4 — Agent Awareness & Communication (Weeks 13–16)
- [ ] Agent Registry with AgentProfile and status tracking
- [ ] Agent Directory injection into task context
- [ ] Agent Message Bus (direct, broadcast, group)
- [ ] Task delegation with downscoped capability inheritance
- [ ] Multi-agent pipeline engine
- [ ] CLI: `agent list/message`, `pipeline run/list`

### Phase 5 — SDKs, Background Tasks & Ecosystem (Weeks 17–20)
- [ ] Rust tool SDK (`agentos-sdk` crate)
- [ ] Python SDK (`agentos` on PyPI)
- [ ] Node.js SDK (`@agentos/sdk` on npm)
- [ ] WASM tool support via Wasmtime
- [ ] Tool Registry (local first, then hosted)
- [ ] `agentd` supervisor and cron scheduler
- [ ] Web UI (Axum + HTMX)

### Phase 6 — Hardening & Release (Weeks 21+)
- [ ] Secrets vault master key bootstrap options (passphrase KDF, TPM)
- [ ] Agent identity persistence across restarts
- [ ] Prompt injection red-teaming
- [ ] External connectivity bridge tools
- [ ] Performance benchmarking and optimization
- [ ] Documentation and public release

---

## Long-Term Vision

If AgentOS succeeds, it becomes the **standard runtime environment for deploying AI agents** — the way Linux became the standard environment for deploying servers.

The long-term trajectory:

1. **AgentOS Core** — the minimal kernel described in this document
2. **AgentOS Cloud** — hosted, multi-tenant deployment with centralized secrets management, permission governance, and fleet-level agent orchestration
3. **AgentOS Marketplace** — rich ecosystem of community tools, hardware drivers, and agent templates
4. **Federated Agent Networks** — multiple AgentOS instances communicating via a federated protocol, with agents delegating tasks across organizational boundaries under capability-scoped trust
5. **AgentOS Edge** — a stripped-down build for embedded and IoT devices where agents directly manage hardware in real time
6. **Formal Verification** — a formally verified capability and permission model for high-stakes deployments in finance, healthcare, and critical infrastructure

The core bet: **as AI agents become the primary way software tasks are executed, the environments they run in will matter as much as the models themselves**. AgentOS is that environment — designed from the ground up, not retrofitted.

---

*AgentOS — an operating system for the age of agents.*

---

> **Status**: Concept / Design Phase
> **Language**: Rust (kernel + core) · Python SDK · Node.js SDK
> **Deployment**: Docker
> **License**: TBD (likely Apache 2.0 or MIT)
