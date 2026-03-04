# Plan 02 — Core Types (`agentos-types` crate)

## Goal

Define all shared data structures used across every crate. This is the leaf crate with zero internal dependencies — everything else depends on it.

## File: `src/ids.rs` — Identity Types

All IDs are UUID v4 wrapped in newtypes for type safety:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::fmt;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(u: Uuid) -> Self {
                Self(u)
            }

            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

define_id!(TaskID);
define_id!(AgentID);
define_id!(ToolID);
define_id!(MessageID);
define_id!(ContextID);
define_id!(TraceID);
define_id!(SecretID);
```

## File: `src/intent.rs` — Intent Message Types

```rust
use crate::ids::*;
use crate::capability::CapabilityToken;
use serde::{Deserialize, Serialize};

/// The core envelope for all communication in AgentOS.
/// Every message between LLM, kernel, and tools uses this format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentMessage {
    pub id: MessageID,
    pub sender_token: CapabilityToken,
    pub intent_type: IntentType,
    pub target: IntentTarget,
    pub payload: SemanticPayload,
    pub context_ref: ContextID,
    pub priority: u8,
    pub timeout_ms: u32,
    pub trace_id: TraceID,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// What kind of action the intent represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentType {
    Read,
    Write,
    Execute,
    Query,
    Observe,
    Delegate,
}

/// Where the intent is directed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentTarget {
    Tool(ToolID),
    Kernel,  // internal kernel operations (memory mgmt, etc.)
}

/// The payload of an intent — validated, schema-checked data.
/// In Phase 1, this is a JSON value. In Phase 2+, this becomes schema-validated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticPayload {
    /// The intent schema name (e.g. "FileReadIntent", "MemorySearchIntent")
    pub schema: String,
    /// The actual data as a JSON value
    pub data: serde_json::Value,
}

/// The result returned by a tool after processing an intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentResult {
    pub intent_id: MessageID,
    pub trace_id: TraceID,
    pub status: IntentResultStatus,
    pub payload: Option<serde_json::Value>,
    pub error: Option<String>,
    pub execution_time_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentResultStatus {
    Success,
    Failed,
    PermissionDenied,
    Timeout,
    ToolNotFound,
    SchemaValidationError,
}
```

## File: `src/capability.rs` — Capability Token Types

```rust
use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// An unforgeable, scoped, kernel-signed token issued to every task.
/// All tool invocations are checked against this token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub allowed_tools: BTreeSet<ToolID>,
    pub allowed_intents: BTreeSet<IntentTypeFlag>,
    pub permissions: PermissionSet,
    pub issued_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// HMAC-SHA256 signature computed by the kernel. Cannot be forged.
    pub signature: Vec<u8>,
}

/// Mirrors IntentType but used in capability tokens for efficient set membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IntentTypeFlag {
    Read,
    Write,
    Execute,
    Query,
    Observe,
    Delegate,
}

/// A set of resource permissions in rwx format.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionSet {
    pub entries: Vec<PermissionEntry>,
}

/// A single permission entry: resource + rwx bits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionEntry {
    /// Resource class, e.g. "fs.user_data", "network.outbound", "memory.semantic"
    pub resource: String,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl PermissionSet {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Check if a specific operation on a resource is allowed.
    pub fn check(&self, resource: &str, operation: PermissionOp) -> bool {
        self.entries.iter().any(|e| {
            e.resource == resource && match operation {
                PermissionOp::Read => e.read,
                PermissionOp::Write => e.write,
                PermissionOp::Execute => e.execute,
            }
        })
    }

    pub fn grant(&mut self, resource: String, read: bool, write: bool, execute: bool) {
        // Upsert: if resource exists, update bits; otherwise add new entry
        if let Some(entry) = self.entries.iter_mut().find(|e| e.resource == resource) {
            entry.read |= read;
            entry.write |= write;
            entry.execute |= execute;
        } else {
            self.entries.push(PermissionEntry { resource, read, write, execute });
        }
    }

    pub fn revoke(&mut self, resource: &str, read: bool, write: bool, execute: bool) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.resource == resource) {
            if read { entry.read = false; }
            if write { entry.write = false; }
            if execute { entry.execute = false; }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PermissionOp {
    Read,
    Write,
    Execute,
}
```

## File: `src/task.rs` — Agent Task Types

```rust
use crate::ids::*;
use crate::capability::CapabilityToken;
use crate::context::ContextWindow;
use crate::intent::IntentMessage;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A single unit of work assigned to an LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: TaskID,
    pub state: TaskState,
    pub agent_id: AgentID,
    pub capability_token: CapabilityToken,
    pub assigned_llm: Option<AgentID>,
    pub priority: u8,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub timeout: Duration,
    pub original_prompt: String,
    pub history: Vec<IntentMessage>,
    pub parent_task: Option<TaskID>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Queued,
    Running,
    Waiting,    // waiting on a tool or sub-agent
    Complete,
    Failed,
}

/// Summary of a task for display purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: TaskID,
    pub state: TaskState,
    pub agent_id: AgentID,
    pub prompt_preview: String,   // first 100 chars of prompt
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tool_calls: u32,
    pub tokens_used: u64,
}
```

## File: `src/context.rs` — Context Window Types

```rust
use crate::ids::*;
use serde::{Deserialize, Serialize};

/// A rolling context window for an agent task.
/// Implemented as a ring buffer with a max entry count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextWindow {
    pub id: ContextID,
    pub entries: Vec<ContextEntry>,
    pub max_entries: usize,
}

/// A single entry in the context window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub role: ContextRole,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub metadata: Option<ContextMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextRole {
    System,
    User,
    Assistant,
    ToolResult,
}

/// Optional metadata attached to a context entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMetadata {
    pub tool_name: Option<String>,
    pub tool_id: Option<ToolID>,
    pub intent_id: Option<MessageID>,
    pub tokens_estimated: Option<u32>,
}

impl ContextWindow {
    pub fn new(max_entries: usize) -> Self {
        Self {
            id: ContextID::new(),
            entries: Vec::new(),
            max_entries,
        }
    }

    /// Push a new entry. If at capacity, evict the oldest non-system entry.
    pub fn push(&mut self, entry: ContextEntry) {
        if self.entries.len() >= self.max_entries {
            // Find the first non-System entry and remove it
            if let Some(idx) = self.entries.iter().position(|e| e.role != ContextRole::System) {
                self.entries.remove(idx);
            }
        }
        self.entries.push(entry);
    }

    /// Get all entries as a slice (for assembling LLM prompts).
    pub fn as_entries(&self) -> &[ContextEntry] {
        &self.entries
    }

    /// Clear all non-system entries.
    pub fn clear_history(&mut self) {
        self.entries.retain(|e| e.role == ContextRole::System);
    }
}
```

## File: `src/tool.rs` — Tool Manifest Types

```rust
use crate::ids::*;
use serde::{Deserialize, Serialize};

/// A tool's manifest, parsed from tool.toml at install time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    pub manifest: ToolInfo,
    pub capabilities_required: ToolCapabilities,
    pub capabilities_provided: ToolOutputs,
    pub intent_schema: ToolSchema,
    pub sandbox: ToolSandbox,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCapabilities {
    pub permissions: Vec<String>,  // e.g. ["fs.read", "context.write"]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutputs {
    pub outputs: Vec<String>,  // e.g. ["content.text", "content.structured"]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub input: String,   // e.g. "FileReadIntent"
    pub output: String,  // e.g. "FileContent"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSandbox {
    pub network: bool,
    pub fs_write: bool,
    pub max_memory_mb: u64,
    pub max_cpu_ms: u64,
}

/// A registered tool in the kernel's tool registry.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    pub id: ToolID,
    pub manifest: ToolManifest,
    pub status: ToolStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Available,
    Running,
    Disabled,
}
```

## File: `src/agent.rs` — Agent Profile Types

```rust
use crate::ids::*;
use crate::capability::PermissionSet;
use serde::{Deserialize, Serialize};

/// Profile of a connected LLM agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: AgentID,
    pub name: String,
    pub provider: LLMProvider,
    pub model: String,
    pub status: AgentStatus,
    pub permissions: PermissionSet,
    pub current_task: Option<TaskID>,
    pub description: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_active: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LLMProvider {
    Ollama,
    OpenAI,
    Anthropic,
    Gemini,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Online,
    Idle,
    Busy,
    Offline,
}
```

## File: `src/secret.rs` — Secret Types

```rust
use crate::ids::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    pub id: SecretID,
    pub name: String,
    pub owner: SecretOwner,
    pub scope: SecretScope,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Note: SecretEntry never contains the actual secret value.
/// The encrypted value lives in the vault DB only.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecretOwner {
    Kernel,
    Agent(AgentID),
    Tool(ToolID),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecretScope {
    Global,
    Agent(AgentID),
    Tool(ToolID),
}

/// Metadata returned to CLI — never includes the actual value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMetadata {
    pub name: String,
    pub scope: SecretScope,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}
```

## File: `src/error.rs` — Error Types

```rust
use thiserror::Error;
use crate::ids::*;

#[derive(Error, Debug)]
pub enum AgentOSError {
    // Kernel errors
    #[error("Task not found: {0}")]
    TaskNotFound(TaskID),

    #[error("Task timed out: {0}")]
    TaskTimeout(TaskID),

    #[error("Kernel is shutting down")]
    KernelShutdown,

    // Capability errors
    #[error("Permission denied: {resource} requires {operation}")]
    PermissionDenied { resource: String, operation: String },

    #[error("Invalid capability token: {reason}")]
    InvalidToken { reason: String },

    #[error("Capability token expired")]
    TokenExpired,

    // Tool errors
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Tool execution failed: {tool_name}: {reason}")]
    ToolExecutionFailed { tool_name: String, reason: String },

    #[error("Schema validation failed: {0}")]
    SchemaValidation(String),

    // LLM errors
    #[error("LLM adapter error: {provider}: {reason}")]
    LLMError { provider: String, reason: String },

    #[error("No LLM connected")]
    NoLLMConnected,

    // Vault errors
    #[error("Secret not found: {0}")]
    SecretNotFound(String),

    #[error("Vault error: {0}")]
    VaultError(String),

    // IPC errors
    #[error("Intent bus error: {0}")]
    BusError(String),

    // Serialization
    #[error("Serialization error: {0}")]
    Serialization(String),

    // IO
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

## File: `src/lib.rs` — Module Exports

```rust
pub mod ids;
pub mod intent;
pub mod capability;
pub mod task;
pub mod context;
pub mod tool;
pub mod agent;
pub mod secret;
pub mod error;

// Re-export commonly used types at crate root
pub use ids::*;
pub use intent::{IntentMessage, IntentType, IntentTarget, IntentResult, IntentResultStatus, SemanticPayload};
pub use capability::{CapabilityToken, PermissionSet, PermissionEntry, PermissionOp};
pub use task::{AgentTask, TaskState, TaskSummary};
pub use context::{ContextWindow, ContextEntry, ContextRole};
pub use tool::{ToolManifest, RegisteredTool, ToolStatus};
pub use agent::{AgentProfile, LLMProvider, AgentStatus};
pub use secret::{SecretEntry, SecretScope, SecretOwner, SecretMetadata};
pub use error::AgentOSError;
```

## Tests to Include

```rust
// In src/context.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_window_push_and_evict() {
        let mut ctx = ContextWindow::new(3);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "You are an agent.".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        ctx.push(ContextEntry {
            role: ContextRole::Assistant,
            content: "Hi!".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        // At capacity — next push should evict oldest non-system entry ("Hello")
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Next message".into(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        });
        assert_eq!(ctx.entries.len(), 3);
        assert_eq!(ctx.entries[0].content, "You are an agent."); // system preserved
        assert_eq!(ctx.entries[1].content, "Hi!");               // second non-system kept
        assert_eq!(ctx.entries[2].content, "Next message");      // newest pushed
    }

    #[test]
    fn test_permission_set_check() {
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".into(), true, false, false);
        perms.grant("network.outbound".into(), false, false, true);

        assert!(perms.check("fs.user_data", PermissionOp::Read));
        assert!(!perms.check("fs.user_data", PermissionOp::Write));
        assert!(perms.check("network.outbound", PermissionOp::Execute));
        assert!(!perms.check("network.outbound", PermissionOp::Read));
        assert!(!perms.check("nonexistent.resource", PermissionOp::Read));
    }
}
```

## Verification

```bash
cd agos
cargo test -p agentos-types    # All unit tests pass
cargo doc -p agentos-types     # Documentation generates
```
