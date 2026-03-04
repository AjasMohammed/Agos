use thiserror::Error;
use crate::ids::*;

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

    // IO
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
