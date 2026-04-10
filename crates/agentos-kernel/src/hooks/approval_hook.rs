use super::Hook;
use crate::escalation::{AutoAction, EscalationManager};
use crate::kernel_action::EscalationReason;
use crate::tool_registry::ToolRegistry;
use agentos_types::{HookEvent, HookResult, RiskClass, TraceID};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Auto-approve rule: matches a specific risk class (and optional path prefix).
#[derive(Debug, Clone)]
pub struct AutoApproveRule {
    /// Risk classes this rule covers.
    pub risk_classes: Vec<RiskClass>,
    /// If set, the tool input's `path` field must start with this prefix.
    /// The prefix is checked against the actual parsed JSON value, not a raw substring.
    pub path_prefix: Option<String>,
    /// Human-readable description for `agentos doctor` output.
    pub description: String,
}

/// Policy that determines which tool calls should be auto-approved vs. escalated.
pub struct AutoApprovePolicy {
    rules: Vec<AutoApproveRule>,
}

impl AutoApprovePolicy {
    /// Default policy: auto-approve all read operations, and writes strictly within /tmp.
    pub fn default_rules() -> Self {
        Self {
            rules: vec![
                AutoApproveRule {
                    risk_classes: vec![RiskClass::ReadonlyScoped, RiskClass::ReadonlyExternal],
                    path_prefix: None,
                    description: "Auto-approve all read operations".to_string(),
                },
                AutoApproveRule {
                    risk_classes: vec![RiskClass::WriteScoped],
                    path_prefix: Some("/tmp/".to_string()),
                    description: "Auto-approve writes strictly under /tmp/".to_string(),
                },
            ],
        }
    }

    /// Returns `true` if this tool call should be auto-approved without human review.
    ///
    /// Path-prefix rules parse the JSON to extract the actual `path` field value,
    /// preventing bypass via crafted JSON strings that merely *contain* the prefix.
    pub fn should_auto_approve(&self, risk_class: &RiskClass, input_json: &str) -> bool {
        if !risk_class.requires_approval() {
            return true; // low-risk operations always pass
        }
        for rule in &self.rules {
            if !rule.risk_classes.contains(risk_class) {
                continue;
            }
            match &rule.path_prefix {
                None => return true, // class match with no path constraint
                Some(prefix) => {
                    // Parse JSON and check the actual `path` field — never do a raw
                    // substring search, which is trivially bypassable via JSON injection.
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(input_json) {
                        if let Some(path) = parsed.get("path").and_then(|v| v.as_str()) {
                            // Reject any path with traversal components before prefix check.
                            // A path like `/tmp/../etc/passwd` starts with `/tmp/` as a string
                            // but resolves outside it — defense-in-depth even if the file tool
                            // also canonicalizes.
                            if path.contains("..") {
                                // Traversal component present — never auto-approve.
                            } else if path.starts_with(prefix.as_str()) {
                                return true;
                            }
                        }
                    }
                    // Fall through to check remaining rules.
                }
            }
        }
        false
    }
}

/// Pre-hook that creates an escalation for high-risk tool calls.
///
/// If the tool's `risk_class` requires approval and the auto-approve policy
/// does not match, this hook creates a `PendingEscalation` and returns
/// `HookResult::Abort` — preventing the tool from executing until a human
/// resolves the escalation via `agentos escalation resolve <id>`.
///
/// **Unknown tools** default to `ExecCapable` (fail-closed) rather than
/// `ReadonlyScoped` (fail-open), following the principle of least privilege.
pub struct ApprovalHook {
    policy: AutoApprovePolicy,
    escalations: Arc<EscalationManager>,
    tool_registry: Arc<RwLock<ToolRegistry>>,
}

impl ApprovalHook {
    pub fn new(
        policy: AutoApprovePolicy,
        escalations: Arc<EscalationManager>,
        tool_registry: Arc<RwLock<ToolRegistry>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            policy,
            escalations,
            tool_registry,
        })
    }
}

#[async_trait]
impl Hook for ApprovalHook {
    fn name(&self) -> &'static str {
        "approval"
    }

    fn handles(&self, event: &HookEvent) -> bool {
        matches!(event, HookEvent::ToolPre { .. })
    }

    async fn on_event(&self, event: &HookEvent) -> HookResult {
        let HookEvent::ToolPre {
            task_id,
            agent_id,
            tool_name,
            input_json,
        } = event
        else {
            return HookResult::Continue;
        };

        // Look up the tool's risk class by name.
        // Unknown tools default to ExecCapable (fail-closed — principle of least privilege).
        let risk_class = {
            let registry = self.tool_registry.read().await;
            registry
                .get_by_name(tool_name)
                .map(|t| t.manifest.risk_class.clone())
                .unwrap_or(RiskClass::ExecCapable)
        };

        // Check auto-approve policy.
        if self.policy.should_auto_approve(&risk_class, input_json) {
            return HookResult::Continue;
        }

        // Create a blocking escalation for human review.
        let summary = format!(
            "Tool '{}' requires approval. Risk class: {:?}. Input preview: {}",
            tool_name,
            risk_class,
            if input_json.chars().count() > 200 {
                format!(
                    "{}…",
                    input_json
                        .char_indices()
                        .nth(200)
                        .map(|(i, _)| &input_json[..i])
                        .unwrap_or(input_json)
                )
            } else {
                input_json.clone()
            }
        );

        let escalation_id = self
            .escalations
            .create_escalation(
                *task_id,
                *agent_id,
                EscalationReason::AuthorizationRequired,
                summary,
                format!(
                    "Tool '{}' (risk: {:?}) awaiting approval",
                    tool_name, risk_class
                ),
                vec!["approve".to_string(), "deny".to_string()],
                "high".to_string(),
                true, // blocking
                TraceID::new(),
                Some(AutoAction::Deny),
            )
            .await;

        // Escalation cap reached: DENY the call (fail-closed).
        // Fail-open here would allow an attacker to bypass approval by flooding the queue.
        if escalation_id == u64::MAX {
            tracing::error!(
                task_id = %task_id,
                tool = %tool_name,
                "Escalation cap reached — BLOCKING tool call for safety"
            );
            return HookResult::Abort(
                "Escalation cap reached — tool call denied for safety. \
                 Resolve pending escalations with `agentos escalation resolve`."
                    .to_string(),
            );
        }

        // NOTE: This abort surfaces as a ToolExecutionFailed error in the task's context.
        // The task continues running — it does not pause. The LLM sees the error and can
        // decide to retry or stop. Full task-pause-on-approval is tracked for a future
        // enhancement (requires scheduler integration in the hook).
        HookResult::Abort(format!(
            "Tool '{}' requires human approval (escalation ID: {}). \
             Run: agentos escalation resolve {} --decision approve",
            tool_name, escalation_id, escalation_id
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_approve_readonly() {
        let policy = AutoApprovePolicy::default_rules();
        assert!(policy.should_auto_approve(&RiskClass::ReadonlyScoped, "{}"));
        assert!(policy.should_auto_approve(&RiskClass::ReadonlyExternal, "{}"));
    }

    #[test]
    fn test_no_auto_approve_exec() {
        let policy = AutoApprovePolicy::default_rules();
        assert!(!policy.should_auto_approve(&RiskClass::ExecCapable, "{}"));
        assert!(!policy.should_auto_approve(&RiskClass::ControlPlane, "{}"));
        assert!(!policy.should_auto_approve(&RiskClass::Interactive, "{}"));
    }

    #[test]
    fn test_auto_approve_write_strictly_in_tmp() {
        let policy = AutoApprovePolicy::default_rules();
        // Actual path starts with /tmp/ → approved
        assert!(policy.should_auto_approve(&RiskClass::WriteScoped, r#"{"path": "/tmp/foo.txt"}"#));
        // Actual path is outside /tmp/ → denied
        assert!(!policy.should_auto_approve(
            &RiskClass::WriteScoped,
            r#"{"path": "/home/user/secret.txt"}"#
        ));
    }

    #[test]
    fn test_path_prefix_not_bypassable_via_json_injection() {
        let policy = AutoApprovePolicy::default_rules();
        // The string "/tmp/" appears in the JSON but NOT in the "path" field.
        // A raw substring check would incorrectly approve this.
        assert!(!policy.should_auto_approve(
            &RiskClass::WriteScoped,
            r#"{"path": "/home/evil.sh", "note": "copying from /tmp/ to here"}"#
        ));
        // Nested path injection attempt
        assert!(!policy.should_auto_approve(
            &RiskClass::WriteScoped,
            r#"{"target": "/home/evil.sh", "source": "/tmp/innocent"}"#
        ));
    }

    #[test]
    fn test_malformed_json_is_denied() {
        let policy = AutoApprovePolicy::default_rules();
        // Malformed JSON for a write: can't parse path → denied
        assert!(!policy.should_auto_approve(&RiskClass::WriteScoped, "not-json"));
    }

    #[test]
    fn test_path_traversal_not_auto_approved() {
        let policy = AutoApprovePolicy::default_rules();
        // Path traversal via ".." should never be auto-approved even if it starts with /tmp/
        assert!(!policy
            .should_auto_approve(&RiskClass::WriteScoped, r#"{"path": "/tmp/../etc/passwd"}"#));
        // Embedded traversal
        assert!(!policy.should_auto_approve(
            &RiskClass::WriteScoped,
            r#"{"path": "/tmp/./../../root/.ssh/authorized_keys"}"#
        ));
    }
}
