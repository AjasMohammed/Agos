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
    /// Create an event subscription at runtime.
    Subscribe,
    /// Remove an existing event subscription at runtime.
    Unsubscribe,
}

impl IntentType {
    /// Map an `IntentType` to the `PermissionOp` used for capability checking.
    ///
    /// Canonical mapping between the intent protocol and the permission model:
    /// - `Query` / `Subscribe` / `Unsubscribe` → `PermissionOp::Query`
    /// - `Observe`                               → `PermissionOp::Observe`
    /// - `Delegate` / `Escalate`                → `PermissionOp::Execute`
    /// - `Message` / `Broadcast`                → `PermissionOp::Write`
    /// - `Read` / `Write` / `Execute`           → direct 1:1 mapping
    pub fn to_permission_op(self) -> crate::capability::PermissionOp {
        use crate::capability::PermissionOp;
        match self {
            IntentType::Read => PermissionOp::Read,
            IntentType::Write => PermissionOp::Write,
            IntentType::Execute => PermissionOp::Execute,
            IntentType::Query => PermissionOp::Query,
            IntentType::Observe => PermissionOp::Observe,
            IntentType::Subscribe => PermissionOp::Query,
            IntentType::Unsubscribe => PermissionOp::Query,
            IntentType::Delegate => PermissionOp::Execute,
            IntentType::Message => PermissionOp::Write,
            IntentType::Broadcast => PermissionOp::Write,
            IntentType::Escalate => PermissionOp::Execute,
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareResource {
    System,
    Process,
    Network,
    LogReader,
    Gpu,
    Storage,
    Sensor,
}

impl HardwareResource {
    /// Returns the HAL driver name for this resource, used to dispatch to the correct driver.
    pub fn as_driver_name(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Process => "process",
            Self::Network => "network",
            Self::LogReader => "log",
            Self::Gpu => "gpu",
            Self::Storage => "storage",
            Self::Sensor => "sensor",
        }
    }
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

/// How long a runtime subscription should remain active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionDuration {
    /// Automatically remove when the current task reaches a terminal state.
    Task,
    /// Keep until explicitly unsubscribed.
    Permanent,
    /// Keep for a fixed TTL in seconds.
    TTL { seconds: u64 },
}

/// Payload for `IntentType::Subscribe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribePayload {
    /// Event filter, e.g. "SecurityEvents.*" or "TaskLifecycle.TaskFailed".
    pub event_filter: String,
    /// Optional payload filter predicate, e.g. "severity == Critical".
    #[serde(default)]
    pub filter_predicate: Option<String>,
    pub duration: SubscriptionDuration,
    /// Optional priority: "critical", "high", "normal", "low".
    #[serde(default)]
    pub priority: Option<String>,
}

/// Payload for `IntentType::Unsubscribe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribePayload {
    pub subscription_id: String,
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
/// Variant order is significant: Autonomous < Notify < SoftApproval < HardApproval < Forbidden
/// (derived Ord — variants are ordered top-to-bottom as written).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::PermissionOp;

    #[test]
    fn test_to_permission_op_direct_mappings() {
        assert_eq!(IntentType::Read.to_permission_op(), PermissionOp::Read);
        assert_eq!(IntentType::Write.to_permission_op(), PermissionOp::Write);
        assert_eq!(
            IntentType::Execute.to_permission_op(),
            PermissionOp::Execute
        );
        assert_eq!(IntentType::Query.to_permission_op(), PermissionOp::Query);
        assert_eq!(
            IntentType::Observe.to_permission_op(),
            PermissionOp::Observe
        );
    }

    #[test]
    fn test_to_permission_op_derived_mappings() {
        // Subscribe and Unsubscribe are read-only interrogation — map to Query
        assert_eq!(
            IntentType::Subscribe.to_permission_op(),
            PermissionOp::Query
        );
        assert_eq!(
            IntentType::Unsubscribe.to_permission_op(),
            PermissionOp::Query
        );
        // Delegate and Escalate launch sub-work — map to Execute
        assert_eq!(
            IntentType::Delegate.to_permission_op(),
            PermissionOp::Execute
        );
        assert_eq!(
            IntentType::Escalate.to_permission_op(),
            PermissionOp::Execute
        );
        // Message and Broadcast produce side-effects — map to Write
        assert_eq!(IntentType::Message.to_permission_op(), PermissionOp::Write);
        assert_eq!(
            IntentType::Broadcast.to_permission_op(),
            PermissionOp::Write
        );
    }
}
