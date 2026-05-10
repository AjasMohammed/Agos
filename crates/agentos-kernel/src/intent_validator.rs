use crate::kernel::Kernel;
use crate::tool_call::ParsedToolCall;
use agentos_types::*;
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

/// Number of consecutive coherence rejections of the same tool that trigger
/// a forced final-synthesis pass (no-tools inference + user-visible nudge).
pub const REJECT_FORCE_END_THRESHOLD: u32 = 2;

/// Tracks per-task tool call history for semantic coherence analysis.
pub struct IntentValidator {
    /// Per-task history of tool calls (tool_name, intent_type, payload hash).
    task_history: RwLock<HashMap<TaskID, Vec<ToolCallRecord>>>,
    /// Per-task per-tool-name count of `Rejected` coherence outcomes. Used to
    /// force task end when small models keep ignoring `kernel_directive: STOP`.
    reject_counts: RwLock<HashMap<TaskID, HashMap<String, u32>>>,
    /// Tasks scheduled for a forced final-synthesis pass on their next iteration.
    /// Drained by `take_force_end_turn`.
    force_end_turn: RwLock<HashSet<TaskID>>,
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
            reject_counts: RwLock::new(HashMap::new()),
            force_end_turn: RwLock::new(HashSet::new()),
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

    /// Increment the rejection counter for `tool_name` on `task_id` and return
    /// the new value. Used to detect models that keep retrying the same denied
    /// tool after the kernel pushed a `kernel_directive: STOP` instruction.
    pub async fn increment_reject_count(&self, task_id: &TaskID, tool_name: &str) -> u32 {
        let mut guard = self.reject_counts.write().await;
        let task_counts = guard.entry(*task_id).or_default();
        let counter = task_counts.entry(tool_name.to_string()).or_insert(0);
        *counter += 1;
        *counter
    }

    /// Mark this task for a forced final-synthesis pass (no-tools inference)
    /// at the start of its next iteration.
    pub async fn mark_force_end_turn(&self, task_id: &TaskID) {
        self.force_end_turn.write().await.insert(*task_id);
    }

    /// Atomically check and clear the force-end flag for this task. Returns
    /// `true` once after `mark_force_end_turn` was set, then `false` until
    /// re-set.
    pub async fn take_force_end_turn(&self, task_id: &TaskID) -> bool {
        self.force_end_turn.write().await.remove(task_id)
    }

    /// Clean up history when a task completes.
    ///
    /// The three maps are cleared sequentially, not atomically. Callers must
    /// guarantee no concurrent `record_tool_call`, `increment_reject_count`,
    /// or `mark_force_end_turn` for `task_id` is in flight — otherwise an
    /// orphan entry could be re-inserted into one map while the others are
    /// already cleared. The kernel's per-task execution is single-threaded so
    /// this invariant holds in practice.
    pub async fn remove_task(&self, task_id: &TaskID) {
        self.task_history.write().await.remove(task_id);
        self.reject_counts.write().await.remove(task_id);
        self.force_end_turn.write().await.remove(task_id);
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

/// Check for looping: same tool + same payload N+ consecutive times.
///
/// `consecutive_same` counts prior matching history entries; the current
/// call is the (consecutive_same + 1)th identical invocation. Threshold
/// of `>= 2` therefore rejects on the 3rd identical call. Small models
/// (e.g. gemma4:31b-cloud) routinely retry an identical tool call after
/// a soft denial — letting that loop continue burns tokens without
/// changing the outcome, so we hard-abort.
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

    // Idempotent read-style intents (Read/Query/Observe) are not hard-rejected on
    // identical repetition — re-polling the same resource is sometimes legitimate
    // (e.g. waiting for a file to appear). They still surface as Suspicious so the
    // audit log captures the pattern. Mutating/messaging/delegating intents have
    // observable side effects and a 3rd identical call almost always indicates a
    // small-model loop, so they are hard-rejected.
    let read_only = matches!(
        tool_call.intent_type,
        IntentType::Read
            | IntentType::Query
            | IntentType::Observe
            | IntentType::Subscribe
            | IntentType::Unsubscribe
    );

    if consecutive_same >= 2 && !read_only {
        return Some(IntentCoherenceResult::Rejected {
            reason: format!(
                "Looping detected: tool '{}' called {} consecutive times with identical payload",
                tool_call.tool_name,
                consecutive_same + 1
            ),
        });
    }

    if consecutive_same >= 1 {
        return Some(IntentCoherenceResult::Suspicious {
            reason: format!(
                "Potential loop: tool '{}' called {} consecutive times with identical payload",
                tool_call.tool_name,
                consecutive_same + 1
            ),
            confidence: 0.6,
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
    for key in &["path", "key", "file", "resource", "target", "url", "scope"] {
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
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: Default::default(),
            spawner_agent_id: None,
            tool_categories: None,
        }
    }

    fn make_tool_call(
        name: &str,
        intent: IntentType,
        payload: serde_json::Value,
    ) -> ParsedToolCall {
        ParsedToolCall {
            id: None,
            tool_name: name.to_string(),
            intent_type: intent,
            payload,
        }
    }

    #[tokio::test]
    async fn test_intent_loop_detection_rejects_repeated_writes() {
        let validator = IntentValidator::new();
        let task = make_task();
        let call = make_tool_call(
            "file-writer",
            IntentType::Write,
            serde_json::json!({"path": "/test", "content": "x"}),
        );

        // Record 3 identical calls
        for _ in 0..3 {
            validator.record_tool_call(&task.id, &call).await;
        }

        let result = validator.validate_coherence(&task, &call).await;
        assert!(matches!(result, IntentCoherenceResult::Rejected { .. }));
    }

    #[tokio::test]
    async fn test_intent_loop_read_only_stays_suspicious() {
        // Read/Query/Observe/Subscribe/Unsubscribe intents are idempotent —
        // re-polling the same resource is sometimes legitimate. Surface as
        // Suspicious for audit but do not hard-reject. Locked per-variant so
        // a future refactor of the `matches!` arm cannot silently drop one.
        let read_only_variants = [
            IntentType::Read,
            IntentType::Query,
            IntentType::Observe,
            IntentType::Subscribe,
            IntentType::Unsubscribe,
        ];

        for intent in read_only_variants {
            let validator = IntentValidator::new();
            let task = make_task();
            let call = make_tool_call("some-tool", intent, serde_json::json!({"path": "/test"}));

            for _ in 0..5 {
                validator.record_tool_call(&task.id, &call).await;
            }

            let result = validator.validate_coherence(&task, &call).await;
            assert!(
                matches!(result, IntentCoherenceResult::Suspicious { .. }),
                "read-only intent {intent:?} must not hard-reject, got {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_intent_loop_mutating_variants_hard_reject() {
        // Side-effecting intents (Write/Execute/Message/Broadcast/Delegate/
        // Escalate) must hard-reject on the 3rd identical call — small models
        // routinely retry these after soft denial and burn tokens.
        let mutating_variants = [
            IntentType::Write,
            IntentType::Execute,
            IntentType::Message,
            IntentType::Broadcast,
            IntentType::Delegate,
            IntentType::Escalate,
        ];

        for intent in mutating_variants {
            let validator = IntentValidator::new();
            let task = make_task();
            let call = make_tool_call("some-tool", intent, serde_json::json!({"target": "x"}));

            // 2 prior identical entries — current call is the 3rd.
            validator.record_tool_call(&task.id, &call).await;
            validator.record_tool_call(&task.id, &call).await;

            let result = validator.validate_coherence(&task, &call).await;
            assert!(
                matches!(result, IntentCoherenceResult::Rejected { .. }),
                "mutating intent {intent:?} must hard-reject on 3rd identical call, got {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_intent_loop_rejects_on_third_identical_call() {
        let validator = IntentValidator::new();
        let task = make_task();
        let call = make_tool_call(
            "agent-message",
            IntentType::Message,
            serde_json::json!({"to": "ghost", "body": "hi"}),
        );

        // 2 prior identical history entries — current is the 3rd identical call.
        validator.record_tool_call(&task.id, &call).await;
        validator.record_tool_call(&task.id, &call).await;

        let result = validator.validate_coherence(&task, &call).await;
        assert!(
            matches!(result, IntentCoherenceResult::Rejected { .. }),
            "3rd identical call must be rejected, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_intent_loop_suspicious_on_second_identical_call() {
        let validator = IntentValidator::new();
        let task = make_task();
        let call = make_tool_call(
            "agent-message",
            IntentType::Message,
            serde_json::json!({"to": "ghost", "body": "hi"}),
        );

        // 1 prior identical entry — current is the 2nd identical call.
        validator.record_tool_call(&task.id, &call).await;

        let result = validator.validate_coherence(&task, &call).await;
        assert!(
            matches!(result, IntentCoherenceResult::Suspicious { .. }),
            "2nd identical call must be suspicious, got {result:?}"
        );
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

    #[tokio::test]
    async fn test_increment_reject_count_per_tool() {
        let validator = IntentValidator::new();
        let task = make_task();

        assert_eq!(validator.increment_reject_count(&task.id, "echo").await, 1);
        assert_eq!(validator.increment_reject_count(&task.id, "echo").await, 2);
        assert_eq!(validator.increment_reject_count(&task.id, "other").await, 1);
    }

    #[tokio::test]
    async fn test_take_force_end_turn_consumes_flag() {
        let validator = IntentValidator::new();
        let task = make_task();

        assert!(!validator.take_force_end_turn(&task.id).await);
        validator.mark_force_end_turn(&task.id).await;
        assert!(validator.take_force_end_turn(&task.id).await);
        assert!(!validator.take_force_end_turn(&task.id).await);
    }

    #[tokio::test]
    async fn test_remove_task_clears_reject_state() {
        let validator = IntentValidator::new();
        let task = make_task();

        validator.increment_reject_count(&task.id, "echo").await;
        validator.mark_force_end_turn(&task.id).await;
        validator.remove_task(&task.id).await;

        assert!(validator.reject_counts.read().await.get(&task.id).is_none());
        assert!(!validator.force_end_turn.read().await.contains(&task.id));
    }
}
