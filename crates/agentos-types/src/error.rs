use crate::ids::*;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentOSError {
    // Kernel errors
    #[error("Task not found: {0}")]
    TaskNotFound(TaskID),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Task timed out: {0}")]
    TaskTimeout(TaskID),

    #[error("Kernel is shutting down")]
    KernelShutdown,

    #[error("Kernel error: {reason}")]
    KernelError { reason: String },

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

    #[error("File '{path}' is locked by agent {holder_agent_id} (task {holder_task_id}), acquired at {acquired_at}")]
    FileLocked {
        path: String,
        holder_agent_id: crate::ids::AgentID,
        holder_task_id: crate::ids::TaskID,
        acquired_at: chrono::DateTime<chrono::Utc>,
    },

    #[error("Tool '{name}' is blocked and cannot be loaded")]
    ToolBlocked { name: String },

    #[error("Tool '{name}' has an invalid manifest signature: {reason}")]
    ToolSignatureInvalid { name: String, reason: String },

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

    #[error("Storage error: {0}")]
    StorageError(String),

    // IPC errors
    #[error("Intent bus error: {0}")]
    BusError(String),

    // HAL errors
    #[error("HAL error: {0}")]
    HalError(String),

    // Sandbox errors
    #[error("Sandbox spawn failed: {reason}")]
    SandboxSpawnFailed { reason: String },

    #[error("Sandbox timeout: tool {tool_name} killed after {timeout_ms}ms")]
    SandboxTimeout { tool_name: String, timeout_ms: u64 },

    #[error("Sandbox seccomp filter error: {reason}")]
    SandboxFilterError { reason: String },

    // Serialization
    #[error("Serialization error: {0}")]
    Serialization(String),

    // Event system
    #[error("Event subscription not found: {0}")]
    EventSubscriptionNotFound(String),

    #[error("Event loop detected at depth {depth} for event type {event_type}")]
    EventLoopDetected { event_type: String, depth: u32 },

    #[error("Event delivery failed: {0}")]
    EventDeliveryFailed(String),

    // IO
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
