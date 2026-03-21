use crate::kernel::Kernel;
use crate::tool_call::ParsedToolCall;
use agentos_types::*;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Tracks per-task tool call history for semantic coherence analysis.
pub struct IntentValidator {
    /// Per-task history of tool calls (tool_name, intent_type, payload hash).
    task_history: RwLock<HashMap<TaskID, Vec<ToolCallRecord>>>,
}

#[derive(Debug, Clone)]
struct ToolCallRecord {
    tool_name: String,
    intent_type: IntentType,
    payload_hash: u64,
    resource_target: Option<String>,
}

impl IntentValidator {
    pub fn new() -> Self {
        Self {
            task_history: RwLock::new(HashMap::new()),
        }
    }

    /// Record a tool call for coherence tracking.
    pub async fn record_tool_call(&self, task_id: &TaskID, tool_call: &ParsedToolCall) {
        let record = ToolCallRecord {
            tool_name: tool_call.tool_name.clone(),
            intent_type: tool_call.intent_type,
            payload_hash: hash_payload(&tool_call.payload),
            resource_target: extract_resource_target(&tool_call.payload),
        };
        self.task_history
            .write()
            .await
            .entry(*task_id)
            .or_default()
            .push(record);
    }

    /// Clean up history when a task completes.
    pub async fn remove_task(&self, task_id: &TaskID) {
        self.task_history.write().await.remove(task_id);
    }

    /// Perform semantic coherence checks on a tool call.
    ///
    /// Layer B validation — runs after structural/capability validation passes.
    /// Returns `Approved` if all checks pass, `Suspicious` or `Rejected` otherwise.
    #[tracing::instrument(skip_all, fields(task_id = %task.id, tool = %tool_call.tool_name))]
    pub async fn validate_coherence(
        &self,
        task: &AgentTask,
        tool_call: &ParsedToolCall,
    ) -> IntentCoherenceResult {
        let history = self.task_history.read().await;
        let records = history.get(&task.id);

        // Rule 1: Intent loop detection — same tool + same payload 3+ times in a row
        if let Some(records) = records {
            if let Some(result) = check_intent_loop(records, tool_call) {
                return result;
            }
        }

        // Rule 2: Write-without-read — agent writes to a resource it never read
        let empty = Vec::new();
        let records_for_write = records.unwrap_or(&empty);
        if let Some(result) = check_write_without_read(records_for_write, tool_call) {
            return result;
        }

        // Rule 3: Scope escalation — agent targets a tool not in its capability token
        if let Some(result) = check_scope_escalation(task, tool_call) {
            return result;
        }

        IntentCoherenceResult::Approved
    }
}

impl Default for IntentValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Check for looping: same tool + same payload 3+ consecutive times.
fn check_intent_loop(
    records: &[ToolCallRecord],
    tool_call: &ParsedToolCall,
) -> Option<IntentCoherenceResult> {
    let current_hash = hash_payload(&tool_call.payload);
    let consecutive_same = records
        .iter()
        .rev()
        .take_while(|r| r.tool_name == tool_call.tool_name && r.payload_hash == current_hash)
        .count();

    if consecutive_same >= 3 {
        return Some(IntentCoherenceResult::Rejected {
            reason: format!(
                "Looping detected: tool '{}' called {} consecutive times with identical payload",
                tool_call.tool_name,
                consecutive_same + 1
            ),
        });
    }

    if consecutive_same >= 2 {
        return Some(IntentCoherenceResult::Suspicious {
            reason: format!(
                "Potential loop: tool '{}' called {} consecutive times with identical payload",
                tool_call.tool_name,
                consecutive_same + 1
            ),
            confidence: 0.7,
        });
    }

    None
}

/// Check for blind overwrite: agent writes to a resource it previously wrote but never
/// read back. First-time writes to new resources are allowed without a prior read.
fn check_write_without_read(
    records: &[ToolCallRecord],
    tool_call: &ParsedToolCall,
) -> Option<IntentCoherenceResult> {
    if tool_call.intent_type != IntentType::Write {
        return None;
    }

    let target = extract_resource_target(&tool_call.payload)?;

    // First write to this resource in this task — no issue.
    let was_previously_written = records.iter().any(|r| {
        r.intent_type == IntentType::Write && r.resource_target.as_deref() == Some(target.as_str())
    });

    if !was_previously_written {
        return None;
    }

    // Resource was written before. Check if it was read back since the last write.
    let write_base = tool_call
        .tool_name
        .replace("-writer", "")
        .replace("-write", "");

    let last_write_idx = records.iter().rposition(|r| {
        r.intent_type == IntentType::Write && r.resource_target.as_deref() == Some(target.as_str())
    });

    if let Some(write_idx) = last_write_idx {
        let was_read_since = records[write_idx..].iter().any(|r| {
            r.intent_type == IntentType::Read
                && r.resource_target.as_deref() == Some(target.as_str())
                && {
                    let read_base = r.tool_name.replace("-reader", "").replace("-read", "");
                    read_base == write_base
                }
        });

        if !was_read_since {
            return Some(IntentCoherenceResult::Suspicious {
                reason: format!(
                    "Blind overwrite: tool '{}' re-writing '{}' without reading it back since last write",
                    tool_call.tool_name, target
                ),
                confidence: 0.5,
            });
        }
    }

    None
}

/// Check for scope escalation via intent type: agent uses an intent type not in its token.
fn check_scope_escalation(
    task: &AgentTask,
    tool_call: &ParsedToolCall,
) -> Option<IntentCoherenceResult> {
    // If allowed_intents is empty, the agent has no intent restrictions (wildcard)
    if task.capability_token.allowed_intents.is_empty() {
        return None;
    }

    let intent_flag = match tool_call.intent_type {
        IntentType::Read => IntentTypeFlag::Read,
        IntentType::Write => IntentTypeFlag::Write,
        IntentType::Execute => IntentTypeFlag::Execute,
        IntentType::Query => IntentTypeFlag::Query,
        IntentType::Observe => IntentTypeFlag::Observe,
        IntentType::Delegate => IntentTypeFlag::Delegate,
        IntentType::Message => IntentTypeFlag::Message,
        IntentType::Broadcast => IntentTypeFlag::Broadcast,
        IntentType::Escalate => IntentTypeFlag::Escalate,
        IntentType::Subscribe => IntentTypeFlag::Subscribe,
        IntentType::Unsubscribe => IntentTypeFlag::Unsubscribe,
    };

    if !task.capability_token.allowed_intents.contains(&intent_flag) {
        return Some(IntentCoherenceResult::Suspicious {
            reason: format!(
                "Scope escalation: intent type '{:?}' not in agent's allowed_intents set",
                tool_call.intent_type
            ),
            confidence: 0.8,
        });
    }

    None
}

/// Extract a resource identifier from a tool payload for comparison purposes.
fn extract_resource_target(payload: &serde_json::Value) -> Option<String> {
    // Try common field names for resource targets
    for key in &[
        "path", "key", "file", "resource", "target", "url", "scope", "name", "id",
    ] {
        if let Some(val) = payload.get(key).and_then(|v| v.as_str()) {
            return Some(val.to_string());
        }
    }
    None
}

/// Simple hash of a JSON payload for deduplication.
fn hash_payload(payload: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let s = payload.to_string();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

impl Kernel {
    /// Combined structural + semantic validation for a tool call.
    ///
    /// Layer A: capability token + schema + permission validation (existing).
    /// Layer B: semantic coherence checks (new).
    #[tracing::instrument(skip_all, fields(task_id = %task.id, tool = %tool_call.tool_name))]
    pub(crate) async fn validate_tool_call_full(
        &self,
        task: &AgentTask,
        tool_call: &ParsedToolCall,
        trace_id: TraceID,
    ) -> Result<IntentCoherenceResult, String> {
        // Layer A: structural validation (existing logic)
        self.validate_tool_call(task, tool_call, trace_id)?;

        // Layer B: semantic coherence
        let coherence = self
            .intent_validator
            .validate_coherence(task, tool_call)
            .await;

        match &coherence {
            IntentCoherenceResult::Suspicious { reason, confidence } => {
                tracing::warn!(
                    task_id = %task.id,
                    tool = %tool_call.tool_name,
                    reason = %reason,
                    confidence = %confidence,
                    "Intent coherence: suspicious"
                );
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::RiskEscalation,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "coherence": "suspicious",
                        "reason": reason,
                        "confidence": confidence,
                        "tool": tool_call.tool_name,
                    }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
            }
            IntentCoherenceResult::Rejected { reason } => {
                tracing::warn!(
                    task_id = %task.id,
                    tool = %tool_call.tool_name,
                    reason = %reason,
                    "Intent coherence: rejected"
                );
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::PermissionDenied,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "coherence": "rejected",
                        "reason": reason,
                        "tool": tool_call.tool_name,
                    }),
                    severity: agentos_audit::AuditSeverity::Security,
                    reversible: false,
                    rollback_ref: None,
                });
            }
            IntentCoherenceResult::Approved => {}
        }

        Ok(coherence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn make_task() -> AgentTask {
        AgentTask {
            id: TaskID::new(),
            state: TaskState::Running,
            agent_id: AgentID::new(),
            capability_token: CapabilityToken {
                task_id: TaskID::new(),
                agent_id: AgentID::new(),
                allowed_tools: BTreeSet::new(),
                allowed_intents: BTreeSet::new(),
                permissions: PermissionSet::new(),
                issued_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now(),
                signature: Vec::new(),
            },
            assigned_llm: None,
            priority: 5,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(300),
            original_prompt: "test task".to_string(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            max_iterations: None,
            trigger_source: None,
            autonomous: false,
        }
    }

    fn make_tool_call(
        name: &str,
        intent: IntentType,
        payload: serde_json::Value,
    ) -> ParsedToolCall {
        ParsedToolCall {
            tool_name: name.to_string(),
            intent_type: intent,
            payload,
        }
    }

    #[tokio::test]
    async fn test_intent_loop_detection() {
        let validator = IntentValidator::new();
        let task = make_task();
        let call = make_tool_call(
            "file-reader",
            IntentType::Read,
            serde_json::json!({"path": "/test"}),
        );

        // Record 3 identical calls
        for _ in 0..3 {
            validator.record_tool_call(&task.id, &call).await;
        }

        let result = validator.validate_coherence(&task, &call).await;
        assert!(matches!(result, IntentCoherenceResult::Rejected { .. }));
    }

    #[tokio::test]
    async fn test_first_write_to_new_resource_approved() {
        let validator = IntentValidator::new();
        let task = make_task();
        let write_call = make_tool_call(
            "file-writer",
            IntentType::Write,
            serde_json::json!({"path": "/data/output.txt", "content": "hello"}),
        );

        let result = validator.validate_coherence(&task, &write_call).await;
        assert!(matches!(result, IntentCoherenceResult::Approved));
    }

    #[tokio::test]
    async fn test_blind_overwrite_suspicious() {
        let validator = IntentValidator::new();
        let task = make_task();
        let write_call = make_tool_call(
            "file-writer",
            IntentType::Write,
            serde_json::json!({"path": "/data/output.txt", "content": "hello"}),
        );

        validator.record_tool_call(&task.id, &write_call).await;

        let result = validator.validate_coherence(&task, &write_call).await;
        assert!(matches!(result, IntentCoherenceResult::Suspicious { .. }));
    }

    #[tokio::test]
    async fn test_write_after_read_approved() {
        let validator = IntentValidator::new();
        let task = make_task();

        let write_call = make_tool_call(
            "file-writer",
            IntentType::Write,
            serde_json::json!({"path": "/data/output.txt", "content": "hello"}),
        );
        validator.record_tool_call(&task.id, &write_call).await;

        let read_call = make_tool_call(
            "file-reader",
            IntentType::Read,
            serde_json::json!({"path": "/data/output.txt"}),
        );
        validator.record_tool_call(&task.id, &read_call).await;

        let result = validator.validate_coherence(&task, &write_call).await;
        assert!(matches!(result, IntentCoherenceResult::Approved));
    }

    #[tokio::test]
    async fn test_approved_for_normal_calls() {
        let validator = IntentValidator::new();
        let task = make_task();
        let call = make_tool_call(
            "file-reader",
            IntentType::Read,
            serde_json::json!({"path": "/test"}),
        );

        let result = validator.validate_coherence(&task, &call).await;
        assert!(matches!(result, IntentCoherenceResult::Approved));
    }

    #[tokio::test]
    async fn test_cleanup_on_task_removal() {
        let validator = IntentValidator::new();
        let task = make_task();
        let call = make_tool_call("file-reader", IntentType::Read, serde_json::json!({}));
        validator.record_tool_call(&task.id, &call).await;

        validator.remove_task(&task.id).await;

        assert!(validator.task_history.read().await.get(&task.id).is_none());
    }
}
