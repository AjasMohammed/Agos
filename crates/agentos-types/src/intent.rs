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
    /// Agent-to-agent direct message.
    Message,
    /// Message to all agents in scope.
    Broadcast,
    /// Request human review / escalation.
    Escalate,
}

/// Where the intent is directed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentTarget {
    Tool(ToolID),
    Kernel,                     // internal kernel operations (memory mgmt, etc.)
    Agent(AgentID),             // direct agent-to-agent messaging
    Hardware(HardwareResource), // HAL-mediated hardware access
    Broadcast,                  // all agents in a group
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

/// Result of semantic coherence checking on an intent.
/// Used by the intent validator to flag suspicious or rejected tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IntentCoherenceResult {
    /// Intent is coherent with task context — proceed.
    Approved,
    /// Intent looks suspicious but not definitively malicious.
    /// Logged to audit; configurable whether to block or warn.
    Suspicious { reason: String, confidence: f32 },
    /// Intent is definitively incoherent — block execution.
    Rejected { reason: String },
}

/// Risk level for an action, determining what approval is required.
/// Based on the AgentOS Action Risk Taxonomy (Spec §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionRiskLevel {
    /// Level 0: Autonomous — no approval needed (read ops, queries).
    Autonomous,
    /// Level 1: Notify — user informed, auto-proceeds.
    Notify,
    /// Level 2: Soft approval — user can cancel within 30s window.
    SoftApproval,
    /// Level 3: Hard approval — explicit confirmation required before execution.
    HardApproval,
    /// Level 4: Forbidden — kernel hard-blocks, no override possible.
    Forbidden,
}
