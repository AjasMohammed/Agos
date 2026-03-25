/// `McpToolAdapter` wraps a single MCP tool as an AgentOS `AgentTool`.
///
/// This is the boundary between the MCP protocol layer and the AgentOS security
/// model. Capability token validation and `PermissionSet` enforcement are applied
/// by the `ToolRunner` *before* `execute()` is called — MCP tools receive the same
/// treatment as native tools.
///
/// The adapter holds an [`McpServerHandle`] rather than a raw `McpClient`.
/// This means transparent reconnection happens at the protocol boundary: if the
/// MCP server process crashes and restarts, agents calling this tool will get a
/// seamless retry without kernel intervention.
use std::sync::Arc;

use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

use crate::handle::McpServerHandle;
use crate::types::McpToolDef;

pub struct McpToolAdapter {
    handle: Arc<McpServerHandle>,
    tool_def: McpToolDef,
    /// The `PermissionSet` resource key required to invoke this tool.
    ///
    /// Defaults to `"mcp.<sanitized_tool_name>"` where the tool name has all
    /// non-alphanumeric/underscore characters replaced with `_`.
    /// Operators may override this per-tool via [`McpToolAdapter::with_permission`].
    permission: String,
}

impl McpToolAdapter {
    /// Wrap an MCP tool definition as an `AgentTool`.
    ///
    /// The default permission resource is `"mcp.<sanitized_tool_name>"`.
    pub fn new(handle: Arc<McpServerHandle>, tool_def: McpToolDef) -> Self {
        let permission = format!("mcp.{}", sanitize_tool_name(&tool_def.name));
        Self {
            handle,
            tool_def,
            permission,
        }
    }

    /// Override the default permission resource key.
    pub fn with_permission(mut self, permission: &str) -> Self {
        self.permission = permission.to_string();
        self
    }
}

/// Sanitize an MCP tool name into a valid AgentOS permission resource component.
///
/// Replaces any character that is not alphanumeric or `_` with `_`.  This
/// prevents tool names containing dots, colons, or spaces from colliding with
/// other permission namespaces (e.g. a tool named `"fs:read"` produces
/// `"mcp.fs_read"` rather than `"mcp.fs:read"`).
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
        payload: Value,
        _context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        // Capability token and permissions were already validated by ToolRunner.
        self.handle
            .call_tool(&self.tool_def.name, payload)
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: self.tool_def.name.clone(),
                reason: e.to_string(),
            })
    }
}
