/// Kernel handler for `KernelCommand::McpStatus`.
///
/// Iterates the live `mcp_handles` stored at boot and collects health
/// information from each `McpServerHandle` without blocking I/O — all
/// fields are either atomic reads or held behind short-duration mutex
/// lock calls.
use agentos_bus::{KernelResponse, McpServerStatus};

use crate::kernel::Kernel;

impl Kernel {
    /// Return the live health status of all configured MCP server connections.
    pub async fn cmd_mcp_status(&self) -> KernelResponse {
        let handles = self.mcp_handles.read().await;
        let mut statuses = Vec::with_capacity(handles.len());
        for handle in handles.iter() {
            statuses.push(McpServerStatus {
                name: handle.server_name().to_string(),
                connected: handle.is_connected().await,
                tool_count: handle.tool_count(),
                last_error: handle.last_error().await,
            });
        }
        KernelResponse::McpServerStatusList(statuses)
    }
}
