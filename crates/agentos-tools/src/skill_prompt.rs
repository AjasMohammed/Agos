use crate::agent_manual::SharedInstalledSkills;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{skill_permission_resource, AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::json;

/// Read-only "fetch a skill's recipe" tool. Returns the installed skill's
/// system prompt text plus its required/optional tools and budget so a chat
/// agent can fold the recipe into its current context without spawning a
/// sub-agent.
///
/// This is the lightweight Phase-1 alternative to `cmd_skill_run` (which
/// would spawn a bounded sub-agent). It does not enforce the skill's tool
/// allowlist or budget — it only surfaces the prompt so the calling agent
/// can read the playbook and act on it within its own caps.
pub struct SkillPromptTool {
    installed_skills: SharedInstalledSkills,
}

impl SkillPromptTool {
    pub fn new(installed_skills: SharedInstalledSkills) -> Self {
        Self { installed_skills }
    }
}

#[async_trait]
impl AgentTool for SkillPromptTool {
    fn name(&self) -> &str {
        "skill-prompt"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Read-only; pulls a public recipe out of the installed-skills snapshot.
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "skill-prompt requires 'name' (the installed skill's name). Call agent-manual {\"section\": \"skills\"} for the inventory.".into(),
                )
            })?;

        // Scope the snapshot to the skills this agent is granted BEFORE any
        // lookup: a skill the operator toggled off must be indistinguishable
        // from one that was never installed, or the "Installed: [...]" hint
        // below leaks the names of skills this agent may not use.
        let snapshot = {
            let guard = self.installed_skills.read().await;
            guard
                .iter()
                .filter(|s| {
                    context
                        .permissions
                        .check(&skill_permission_resource(&s.name), PermissionOp::Execute)
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        if snapshot.is_empty() {
            // PermissionDenied, not a generic failure: this is the shape the
            // audit log and the approval surfaces key on, and "none installed"
            // vs "none granted" is deliberately not distinguished.
            return Err(AgentOSError::PermissionDenied {
                resource: skill_permission_resource(name),
                operation: "execute".into(),
            });
        }

        let matched = snapshot
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| {
                let known: Vec<&str> = snapshot.iter().map(|s| s.name.as_str()).collect();
                AgentOSError::ToolExecutionFailed {
                    tool_name: "skill-prompt".into(),
                    reason: format!(
                        "skill '{name}' not available to you. Available: [{}] \
                         (a skill the operator scoped out is not listed)",
                        known.join(", ")
                    ),
                }
            })?;

        Ok(json!({
            "name": matched.name,
            "version": matched.version,
            "description": matched.description,
            "trust_tier": matched.trust_tier,
            "tools": {
                "required": matched.tools_required,
                "optional": matched.tools_optional,
            },
            "budget": {
                "max_cost_per_run": matched.max_cost_per_run,
                "max_tokens_per_run": matched.max_tokens_per_run,
            },
            // `system_prompt` is `Arc<str>`; `as_ref()` lands `&str` for
            // serde_json — avoids cloning the prose into a String just to
            // serialize it.
            "system_prompt": matched.system_prompt.as_ref(),
            "usage": "Apply the system_prompt as guidance for the user's request. \
                     Stay within the listed required tools where possible — they \
                     are the recipe's allowlist. Budget is advisory at this layer."
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_manual::{SharedInstalledSkills, SkillSummary};
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn skill(name: &str, prompt: &str) -> SkillSummary {
        SkillSummary {
            name: name.into(),
            version: "0.1.0".into(),
            description: format!("{name} skill"),
            author: "test".into(),
            trust_tier: "core".into(),
            roles: vec![],
            schedule: None,
            events: vec![],
            tools_required: vec!["notify-user".into()],
            tools_optional: vec![],
            permissions_required: vec![],
            max_cost_per_run: 0.05,
            max_tokens_per_run: 8000,
            system_prompt: prompt.into(),
        }
    }

    /// Default agent scope: the broad `skill:` execute grant every agent is
    /// registered with, which covers every installed skill.
    fn ctx() -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant_op("skill:".to_string(), PermissionOp::Execute, None);
        ctx_with(permissions)
    }

    fn ctx_with(permissions: PermissionSet) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions,
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

    fn snapshot(skills: Vec<SkillSummary>) -> SharedInstalledSkills {
        Arc::new(RwLock::new(skills))
    }

    #[tokio::test]
    async fn returns_full_prompt_for_installed_skill() {
        let snap = snapshot(vec![skill("alert-builder", "You are the Alert Builder.")]);
        let tool = SkillPromptTool::new(snap);
        let result = tool
            .execute(json!({"name": "alert-builder"}), ctx())
            .await
            .unwrap();
        assert_eq!(result["name"], "alert-builder");
        assert_eq!(result["system_prompt"], "You are the Alert Builder.");
        assert_eq!(result["budget"]["max_tokens_per_run"], 8000);
        let req: Vec<&str> = result["tools"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(req.contains(&"notify-user"));
    }

    #[tokio::test]
    async fn name_match_is_case_insensitive() {
        let snap = snapshot(vec![skill("alert-builder", "prompt body")]);
        let tool = SkillPromptTool::new(snap);
        let result = tool
            .execute(json!({"name": "ALERT-BUILDER"}), ctx())
            .await
            .unwrap();
        assert_eq!(result["name"], "alert-builder");
    }

    #[tokio::test]
    async fn missing_name_field_returns_schema_error() {
        let snap = snapshot(vec![skill("alert-builder", "x")]);
        let tool = SkillPromptTool::new(snap);
        let err = tool.execute(json!({}), ctx()).await.unwrap_err();
        match err {
            AgentOSError::SchemaValidation(_) => {}
            other => panic!("expected SchemaValidation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_skill_lists_installed_options() {
        let snap = snapshot(vec![
            skill("alert-builder", "x"),
            skill("cost-optimizer", "y"),
        ]);
        let tool = SkillPromptTool::new(snap);
        let err = tool
            .execute(json!({"name": "not-a-skill"}), ctx())
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not available"));
        assert!(msg.contains("alert-builder"));
        assert!(msg.contains("cost-optimizer"));
    }

    #[tokio::test]
    async fn skill_without_grant_is_refused() {
        let snap = snapshot(vec![skill("alert-builder", "x")]);
        let tool = SkillPromptTool::new(snap);
        let err = tool
            .execute(
                json!({"name": "alert-builder"}),
                ctx_with(PermissionSet::new()),
            )
            .await
            .unwrap_err();
        // Classified as a permission denial (not a generic tool failure) so it
        // audits and surfaces like every other denied capability.
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn denied_skill_is_neither_returned_nor_named() {
        // Toggled off for this agent: must look exactly like "not installed",
        // or the error message leaks the operator's scoping decision.
        let mut permissions = PermissionSet::new();
        permissions.grant_op("skill:".to_string(), PermissionOp::Execute, None);
        permissions.deny("skill:cost-optimizer/".to_string());
        let snap = snapshot(vec![
            skill("alert-builder", "x"),
            skill("cost-optimizer", "y"),
        ]);
        let tool = SkillPromptTool::new(snap);
        let err = tool
            .execute(json!({"name": "cost-optimizer"}), ctx_with(permissions))
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not available"));
        assert!(msg.contains("alert-builder"));
        assert!(
            msg.contains("Available: [alert-builder]"),
            "denied skill must not be listed as available: {msg}"
        );
    }

    #[tokio::test]
    async fn empty_registry_returns_clear_error() {
        let snap = snapshot(vec![]);
        let tool = SkillPromptTool::new(snap);
        let err = tool
            .execute(json!({"name": "anything"}), ctx())
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Permission denied"));
        assert!(msg.contains("skill:anything/"));
    }
}
