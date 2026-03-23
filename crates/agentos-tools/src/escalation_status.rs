use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct EscalationStatusTool;

impl EscalationStatusTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EscalationStatusTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EscalationStatusTool {
    fn name(&self) -> &str {
        "escalation-status"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("escalation.query".to_string(), PermissionOp::Query)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("escalation.query", PermissionOp::Query)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "escalation.query".to_string(),
                operation: "Query".to_string(),
            });
        }

        let query =
            context
                .escalation_query
                .as_ref()
                .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                    tool_name: "escalation-status".into(),
                    reason: "Escalation query not available in this context".into(),
                })?;

        // If an ID is specified, return that single escalation
        if let Some(id_val) = payload.get("id") {
            let id = id_val.as_u64().ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "escalation-status 'id' must be a non-negative integer".into(),
                )
            })?;
            return match query.get_escalation(id) {
                Some(e) => {
                    // Defense-in-depth: the snapshot should already be scoped to this
                    // agent (see task_executor.rs), but we re-check here in case a
                    // non-snapshot EscalationQuery implementation is injected.
                    if e.agent_id != context.agent_id {
                        return Ok(serde_json::json!({
                            "found": false,
                            "id": id,
                            "message": format!("No escalation found with id {}", id),
                        }));
                    }
                    Ok(serde_json::json!({
                        "found": true,
                        "note": "Snapshot reflects state at task-dispatch time; resolution state may be stale.",
                        "escalation": {
                            "id": e.id,
                            "task_id": e.task_id.to_string(),
                            "agent_id": e.agent_id.to_string(),
                            "reason": e.reason,
                            "context_summary": e.context_summary,
                            "decision_point": e.decision_point,
                            "options": e.options,
                            "urgency": e.urgency,
                            "blocking": e.blocking,
                            "created_at": e.created_at.to_rfc3339(),
                            "expires_at": e.expires_at.to_rfc3339(),
                            "resolved": e.resolved,
                            "resolution": e.resolution,
                        }
                    }))
                }
                None => Ok(serde_json::json!({
                    "found": false,
                    "id": id,
                    "message": format!("No escalation found with id {}", id),
                })),
            };
        }

        // Default: list pending escalations for this agent
        let pending = query.list_pending_for_agent(&context.agent_id);
        let serialized: Vec<_> = pending
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "task_id": e.task_id.to_string(),
                    "reason": e.reason,
                    "decision_point": e.decision_point,
                    "urgency": e.urgency,
                    "blocking": e.blocking,
                    "created_at": e.created_at.to_rfc3339(),
                    "expires_at": e.expires_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "count": serialized.len(),
            "note": "Snapshot reflects state at task-dispatch time; resolution state may be stale.",
            "pending_escalations": serialized,
        }))
    }
}
