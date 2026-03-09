---
title: Type System
tags: [architecture, types]
---

# Type System

All core types live in `agentos-types` (`crates/agentos-types/src/`). This crate has zero internal dependencies and is used by every other crate.

## ID Types

All IDs are strongly-typed UUID v4 wrappers generated via macro:

```rust
TaskID, AgentID, ToolID, MessageID, ContextID,
TraceID, SecretID, RoleID, GroupID, ScheduleID, RunID
```

Each implements `Display`, `Default` (random UUID), `Serialize`, `Deserialize`, `Clone`, `Hash`, `Eq`.

## Agent Types

### AgentProfile
```rust
pub struct AgentProfile {
    pub id: AgentID,
    pub name: String,
    pub provider: LLMProvider,    // Ollama | OpenAI | Anthropic | Gemini | Custom
    pub model: String,
    pub status: AgentStatus,      // Online | Idle | Busy | Offline
    pub permissions: PermissionSet,
    pub roles: Vec<String>,
    pub current_task: Option<TaskID>,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}
```

### LLMProvider
```rust
pub enum LLMProvider {
    Ollama,
    OpenAI,
    Anthropic,
    Gemini,
    Custom(String),
}
```

## Task Types

### AgentTask
```rust
pub struct AgentTask {
    pub id: TaskID,
    pub state: TaskState,           // Queued | Running | Waiting | Complete | Failed | Cancelled
    pub agent_id: AgentID,
    pub capability_token: CapabilityToken,
    pub assigned_llm: Option<AgentID>,
    pub priority: u8,
    pub created_at: DateTime<Utc>,
    pub timeout: Duration,
    pub original_prompt: String,
    pub history: Vec<IntentMessage>,
    pub parent_task: Option<TaskID>,
}
```

### TaskState
```
Queued → Running → Complete
                 → Failed
                 → Waiting → Running (loop)
Any → Cancelled
```

## Intent Types

### IntentMessage
```rust
pub struct IntentMessage {
    pub id: MessageID,
    pub sender_token: CapabilityToken,
    pub intent_type: IntentType,     // Read | Write | Execute | Query | Observe | Delegate
    pub target: IntentTarget,        // Tool(ToolID) | Kernel
    pub payload: SemanticPayload,    // { schema: String, data: Value }
    pub context_ref: ContextID,
    pub priority: u8,
    pub timeout_ms: u32,
    pub trace_id: TraceID,
    pub timestamp: DateTime<Utc>,
}
```

## Capability Types

### CapabilityToken
```rust
pub struct CapabilityToken {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub allowed_tools: BTreeSet<ToolID>,
    pub allowed_intents: BTreeSet<IntentTypeFlag>,
    pub permissions: PermissionSet,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub signature: Vec<u8>,  // HMAC-SHA256
}
```

### PermissionSet
Collection of `PermissionEntry`:
```rust
pub struct PermissionEntry {
    pub resource: String,     // e.g., "fs.user_data"
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub expires_at: Option<DateTime<Utc>>,
}
```

## Error Types

```rust
pub enum AgentOSError {
    Config(String),
    Audit(String),
    Vault(String),
    Capability(String),
    Bus(String),
    Kernel(String),
    Tool(String),
    Sandbox(String),
    LLM(String),
    NotFound(String),
    PermissionDenied(String),
    Timeout(String),
    Internal(String),
}
```

## Schedule Types

```rust
pub struct ScheduledJob {
    pub id: ScheduleID,
    pub name: String,
    pub cron_expr: String,
    pub agent_name: String,
    pub task_template: String,
    pub status: ScheduleStatus,  // Active | Paused | Deleted
    pub created_at: DateTime<Utc>,
    pub last_fired: Option<DateTime<Utc>>,
    pub next_fire: Option<DateTime<Utc>>,
}
```

## Tool Types

```rust
pub struct ToolManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub checksum: Option<String>,
    pub capabilities_required: CapabilitiesRequired,
    pub capabilities_provided: CapabilitiesProvided,
    pub intent_schema: IntentSchema,
    pub sandbox: SandboxConstraints,
    pub executor: Option<ExecutorConfig>,
}
```
