use crate::capability::CapabilityToken;
use crate::ids::*;
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
    Kernel,                          // internal kernel operations (memory mgmt, etc.)
    Agent(AgentID),                  // direct agent-to-agent messaging
    Hardware(HardwareResource),      // HAL-mediated hardware access
    Broadcast,                       // all agents in a group
}

/// Hardware resource categories accessible via the HAL.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HardwareResource {
    System,
    Process,
    Network,
    LogReader,
}

/// The payload of an intent — schema-validated data.
/// When a tool provides an `input_schema` in its manifest, the kernel's
/// `SchemaRegistry` validates `data` against it before execution.
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
