//! `procedure-run` — execute a stored executable procedure.
//!
//! A stub: it validates the shape of the call and hands a `_kernel_action` back
//! to the task executor, which dispatches it. Running a procedure needs the
//! procedural store, the tool registry, the capability engine and the pipeline
//! engine — none of which a `ToolExecutionContext` carries — so the work lives
//! in `agentos_kernel::commands::procedure`.

use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct ProcedureRun;

impl ProcedureRun {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProcedureRun {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ProcedureRun {
    fn name(&self) -> &str {
        "procedure-run"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Permission to READ the recipe. Authority to perform each step comes
        // from that step's own capability token, minted from this agent's
        // PermissionSet — so writing an ambitious procedure grants nothing.
        vec![("memory.procedural".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let procedure = payload
            .get("procedure")
            .or_else(|| payload.get("name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "procedure-run requires 'procedure' (the stored procedure's name)".into(),
                )
            })?;
        let inputs = payload
            .get("inputs")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if !matches!(
            inputs,
            serde_json::Value::Null | serde_json::Value::Object(_)
        ) {
            return Err(AgentOSError::SchemaValidation(
                "procedure-run: 'inputs' must be an object of parameter values".into(),
            ));
        }

        Ok(serde_json::json!({
            "_kernel_action": "run_procedure",
            "procedure": procedure,
            "inputs": inputs,
            "detach": payload.get("detach").and_then(|v| v.as_bool()).unwrap_or(false),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use serde_json::json;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: std::env::temp_dir(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
            shared_dir: None,
        }
    }

    #[tokio::test]
    async fn emits_a_kernel_action() {
        let out = ProcedureRun::new()
            .execute(
                json!({ "procedure": "speak-aloud", "inputs": { "text": "hi" } }),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(out["_kernel_action"], "run_procedure");
        assert_eq!(out["procedure"], "speak-aloud");
        assert_eq!(out["inputs"]["text"], "hi");
        assert_eq!(out["detach"], false);
    }

    /// Models reach for `name` as often as `procedure`; accepting both costs
    /// nothing and saves a failed turn.
    #[tokio::test]
    async fn name_is_accepted_as_an_alias() {
        let out = ProcedureRun::new()
            .execute(json!({ "name": "speak-aloud" }), ctx())
            .await
            .unwrap();
        assert_eq!(out["procedure"], "speak-aloud");
        assert_eq!(out["inputs"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn bad_input_is_rejected() {
        for payload in [
            json!({}),
            json!({ "procedure": "  " }),
            json!({ "procedure": "p", "inputs": [1, 2] }),
            json!({ "procedure": "p", "inputs": "text" }),
        ] {
            let err = ProcedureRun::new()
                .execute(payload.clone(), ctx())
                .await
                .unwrap_err();
            assert!(
                matches!(err, AgentOSError::SchemaValidation(_)),
                "{payload} → {err}"
            );
        }
    }
}
