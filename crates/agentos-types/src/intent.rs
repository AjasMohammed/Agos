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
