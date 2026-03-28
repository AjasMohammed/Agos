use crate::ids::*;
use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum AgentOSError {
    // Kernel errors
    #[error("Task not found: {0}")]
    TaskNotFound(TaskID),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Public key already registered for agent {agent_id} — re-registration rejected")]
    PubkeyAlreadyRegistered { agent_id: String },

    #[error("Task timed out: {0}")]
    TaskTimeout(TaskID),

    #[error("Budget exceeded for agent {agent_id}: {detail}")]
    BudgetExceeded { agent_id: String, detail: String },

    #[error("Rate limited: {detail}")]
    RateLimited { detail: String },

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

    #[error("Device '{0}' is quarantined and access is permanently denied")]
    DeviceQuarantined(String),

    #[error("Device '{device_id}' access is pending approval (escalation: {escalation_id})")]
    DeviceAccessPending {
        device_id: String,
        escalation_id: String,
    },

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

    // RPC errors
    #[error("RPC call depth exceeded: max {max} nested calls allowed")]
    RpcDepthExceeded { max: u32 },

    #[error("RPC call aborted: caller channel dropped")]
    RpcAborted,

    #[error("RPC call failed: {0}")]
    RpcFailed(String),

    // IO — wrapped in Arc so AgentOSError is Clone while preserving the error source chain.
    #[error("IO error: {0}")]
    Io(#[source] std::sync::Arc<std::io::Error>),
}

impl From<std::io::Error> for AgentOSError {
    fn from(e: std::io::Error) -> Self {
        AgentOSError::Io(std::sync::Arc::new(e))
    }
}
