/// `McpToolAdapter` wraps a single MCP tool as an AgentOS `AgentTool`,
/// delegating through the supervisor (transport) and security gate (validation).
///
/// Flow: rate limit check -> supervisor.call_tool -> output validation -> audit log.
use std::sync::Arc;

use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

use crate::security::McpSecurityGate;
use crate::supervisor::McpSupervisor;
use crate::types::McpToolDef;

pub struct McpToolAdapter {
    supervisor: Arc<McpSupervisor>,
    security_gate: Arc<McpSecurityGate>,
    server_name: String,
    tool_def: McpToolDef,
    permission: String,
}

impl McpToolAdapter {
    /// Create a new adapter.
    pub fn new(
        supervisor: Arc<McpSupervisor>,
        security_gate: Arc<McpSecurityGate>,
        server_name: String,
        tool_def: McpToolDef,
    ) -> Self {
        let permission = format!("mcp.{}", sanitize_tool_name(&tool_def.name));
        Self {
            supervisor,
            security_gate,
            server_name,
            tool_def,
            permission,
        }
    }
}

/// Sanitize an MCP tool name into a valid AgentOS permission resource component.
fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[async_trait]
impl AgentTool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.tool_def.name
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![(self.permission.clone(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let start = tokio::time::Instant::now();
        let input_size = serde_json::to_string(&payload)
            .map(|s| s.len())
            .unwrap_or(0);

        // Step 1: Check rate limit and tool whitelist.
        self.security_gate
            .check_tool_allowed(&self.server_name, &self.tool_def.name)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: self.tool_def.name.clone(),
                reason: e,
            })?;

        // Step 2: Call the tool via supervisor.
        let result = match self
            .supervisor
            .call_tool(&self.server_name, &self.tool_def.name, payload)
            .await
        {
            Ok(val) => val,
            Err(e) => {
                let latency_ms = start.elapsed().as_millis() as u64;
                self.security_gate.audit_tool_call(
                    &self.server_name,
                    &self.tool_def.name,
                    input_size,
                    0,
                    latency_ms,
                    false,
                    context.trace_id,
                    Some(context.task_id),
                    Some(context.agent_id),
                );
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: self.tool_def.name.clone(),
                    reason: e.to_string(),
                });
            }
        };

        // Step 3: Validate and wrap output.
        let output_size_before = serde_json::to_string(&result).map(|s| s.len()).unwrap_or(0);
        let wrapped = match self
            .security_gate
            .process_output(result, &self.server_name)
            .await
        {
            Ok(val) => val,
            Err(e) => {
                let latency_ms = start.elapsed().as_millis() as u64;
                self.security_gate.audit_tool_call(
                    &self.server_name,
                    &self.tool_def.name,
                    input_size,
                    output_size_before,
                    latency_ms,
                    false,
                    context.trace_id,
                    Some(context.task_id),
                    Some(context.agent_id),
                );
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: self.tool_def.name.clone(),
                    reason: e,
                });
            }
        };

        // Step 4: Audit the successful call.
        let latency_ms = start.elapsed().as_millis() as u64;
        let output_size = serde_json::to_string(&wrapped)
            .map(|s| s.len())
            .unwrap_or(output_size_before);
        self.security_gate.audit_tool_call(
            &self.server_name,
            &self.tool_def.name,
            input_size,
            output_size,
            latency_ms,
            true,
            context.trace_id,
            Some(context.task_id),
            Some(context.agent_id),
        );

        Ok(wrapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_tool_name_handles_special_chars() {
        assert_eq!(sanitize_tool_name("read-file"), "read_file");
        assert_eq!(sanitize_tool_name("read:file"), "read_file");
        assert_eq!(sanitize_tool_name("read file"), "read_file");
        assert_eq!(sanitize_tool_name("read_file"), "read_file");
    }
}
