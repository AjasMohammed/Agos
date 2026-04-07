use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Wrap user-supplied content in `<user_data>` tags to prevent prompt injection.
/// Escapes tag boundaries that could allow an attacker to break out of the
/// `<user_data>` envelope or the `<output_to_verify>` wrapper used by the
/// verification prompt.
fn sanitize_user_data(value: &str) -> String {
    let safe = value
        .replace("<user_data>", "&lt;user_data&gt;")
        .replace("</user_data>", "&lt;/user_data&gt;")
        .replace("<output_to_verify>", "&lt;output_to_verify&gt;")
        .replace("</output_to_verify>", "&lt;/output_to_verify&gt;");
    format!("<user_data>{safe}</user_data>")
}

/// Verify an output by spawning a second agent as a critic/reviewer.
/// Uses the `_kernel_action: "spawn_agent"` pattern with a verification-
/// specific prompt wrapper so the verifier agent focuses on correctness,
/// safety, and quality of the provided output.
pub struct VerifyOutputTool;

impl VerifyOutputTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for VerifyOutputTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for VerifyOutputTool {
    fn name(&self) -> &str {
        "verify-output"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.spawn".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let agent = payload
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("verify-output requires 'agent' field".into())
            })?;
        let output = payload
            .get("output")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("verify-output requires 'output' field".into())
            })?;
        let criteria = payload
            .get("criteria")
            .and_then(|v| v.as_str())
            .unwrap_or("correctness, safety, and completeness");

        // Sanitize user-supplied content to prevent prompt injection.
        // Wrap in <user_data> tags and escape any existing tag boundaries,
        // consistent with the pipeline engine's sanitize_for_prompt pattern.
        let safe_output = sanitize_user_data(output);
        let safe_criteria = sanitize_user_data(criteria);

        let prompt = format!(
            "You are a verification agent. Review the following output and evaluate it \
             against these criteria: {safe_criteria}.\n\n\
             <output_to_verify>\n{safe_output}\n</output_to_verify>\n\n\
             Respond with a JSON object: {{\"verdict\": \"pass\" | \"fail\" | \"needs_revision\", \
             \"issues\": [\"...\"], \"summary\": \"...\"}}"
        );

        tracing::debug!(
            agent = %agent,
            criteria = %criteria,
            output_len = output.len(),
            "verify_output tool: forwarding verification to kernel"
        );

        Ok(serde_json::json!({
            "_kernel_action": "spawn_agent",
            "agent": agent,
            "prompt": prompt,
            "permissions": [],
            "context_messages": 0,
        }))
    }
}

/// Spawn a sub-agent to handle a specific subtask.
/// Uses the `_kernel_action: "spawn_agent"` pattern so the kernel intercepts
/// the result and performs the privileged spawn operation.
pub struct SpawnAgentTool;

impl SpawnAgentTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SpawnAgentTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn-agent"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.spawn".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let agent = payload
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("spawn-agent requires 'agent' field".into())
            })?;
        let prompt = payload
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("spawn-agent requires 'prompt' field".into())
            })?;

        let permissions: Vec<serde_json::Value> = payload
            .get("permissions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Clamp context_messages to the schema maximum (100) to prevent
        // excessive context cloning even if schema validation is bypassed.
        let context_messages = payload
            .get("context_messages")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(100);

        tracing::debug!(
            agent = %agent,
            permissions_count = permissions.len(),
            context_messages = context_messages,
            "spawn_agent tool: forwarding to kernel"
        );

        Ok(serde_json::json!({
            "_kernel_action": "spawn_agent",
            "agent": agent,
            "prompt": prompt,
            "permissions": permissions,
            "context_messages": context_messages,
        }))
    }
}

/// Wait for spawned sub-agents to complete and retrieve their results.
/// Uses the `_kernel_action: "await_agents"` pattern.
pub struct AwaitAgentsTool;

impl AwaitAgentsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AwaitAgentsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AwaitAgentsTool {
    fn name(&self) -> &str {
        "await-agents"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.spawn".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let task_ids: Vec<serde_json::Value> = payload
            .get("task_ids")
            .and_then(|v| v.as_array())
            .cloned()
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("await-agents requires 'task_ids' array".into())
            })?;

        if task_ids.is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "await-agents: task_ids must not be empty".into(),
            ));
        }

        tracing::debug!(
            task_count = task_ids.len(),
            "await_agents tool: forwarding to kernel"
        );

        Ok(serde_json::json!({
            "_kernel_action": "await_agents",
            "task_ids": task_ids,
        }))
    }
}
