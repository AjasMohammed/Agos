/// `agentctl mcp` — MCP (Model Context Protocol) adapter commands.
///
/// Subcommands:
///   `serve`  — Expose registered AgentOS tools as an MCP server on stdio.
///              Intended for use with Claude Desktop, Cursor, or any MCP client.
///   `list`   — List MCP server connections defined in the current config.
///   `status` — Show live connection health for all configured MCP servers.
use std::sync::Arc;

use agentos_bus::{BusClient, KernelCommand, KernelResponse};
use agentos_mcp::{McpServer, McpToolDef, McpToolExecutor};
use agentos_tools::runner::ToolRunner;
use async_trait::async_trait;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum McpCommands {
    /// Expose all registered AgentOS tools as an MCP server over stdin/stdout.
    ///
    /// Pipe MCP JSON-RPC requests on stdin; responses are written to stdout.
    /// This allows Claude Desktop, Cursor, and other MCP-compatible clients to
    /// use AgentOS tools directly.
    ///
    /// Example (test from shell):
    ///   echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | agentctl mcp serve
    Serve,

    /// List MCP server connections configured in the kernel config file.
    List,

    /// Show live connection health for all configured MCP servers.
    ///
    /// Requires a running kernel. Reports each server's name, connection
    /// state, registered tool count, and last error (if any).
    Status,
}

/// Run the requested MCP subcommand.
///
/// `serve` and `list` are offline commands (no bus connection needed).
/// `status` requires a running kernel.
pub async fn handle(command: McpCommands, config_path: &str) -> anyhow::Result<()> {
    match command {
        McpCommands::Serve => cmd_serve(config_path).await,
        McpCommands::List => cmd_list(config_path),
        McpCommands::Status => anyhow::bail!("mcp status requires a running kernel"),
    }
}

// ── status ────────────────────────────────────────────────────────────────────

/// Query the kernel for live MCP server health and print a table.
pub async fn cmd_mcp_status(bus: &mut BusClient) -> anyhow::Result<()> {
    match bus.send_command(KernelCommand::McpStatus).await? {
        KernelResponse::McpServerStatusList(list) => {
            if list.is_empty() {
                println!("No MCP servers configured.");
                return Ok(());
            }
            println!("{:<20} {:<12} {:<8} LAST ERROR", "NAME", "STATUS", "TOOLS");
            println!("{}", "-".repeat(70));
            for s in list {
                let status = if s.connected {
                    "connected"
                } else {
                    "disconnected"
                };
                let err = s.last_error.as_deref().unwrap_or("-");
                println!("{:<20} {:<12} {:<8} {}", s.name, status, s.tool_count, err);
            }
        }
        KernelResponse::Error { message } => {
            anyhow::bail!("Kernel error: {}", message);
        }
        other => {
            anyhow::bail!("Unexpected response: {:?}", other);
        }
    }
    Ok(())
}

// ── serve ─────────────────────────────────────────────────────────────────────

/// Boot a `ToolRunner` from the config, then serve all registered tools as an
/// MCP server over stdin/stdout.
async fn cmd_serve(config_path: &str) -> anyhow::Result<()> {
    let config = agentos_kernel::config::load_config(std::path::Path::new(config_path))?;
    let data_dir = std::path::PathBuf::from(&config.tools.data_dir);

    // Load a minimal ToolRunner with the core tool set.
    // We do not boot memory stores, LLMs, or the full kernel — only tools.
    let tool_runner = Arc::new(ToolRunner::new(&data_dir).map_err(|e| anyhow::anyhow!(e))?);

    let executor = Arc::new(ToolRunnerExecutor {
        runner: tool_runner,
        data_dir,
    });
    let server = McpServer::new(executor);

    eprintln!("AgentOS MCP server running on stdio. Send JSON-RPC 2.0 requests on stdin.");
    server.serve_stdio().await?;
    Ok(())
}

// ── list ──────────────────────────────────────────────────────────────────────

fn cmd_list(config_path: &str) -> anyhow::Result<()> {
    let config = agentos_kernel::config::load_config(std::path::Path::new(config_path))?;

    if config.mcp.servers.is_empty() {
        println!("No MCP servers configured.");
        println!();
        println!("To add one, edit your config file and add:");
        println!("  [[mcp.servers]]");
        println!("  name = \"filesystem\"");
        println!("  command = \"npx\"");
        println!("  args = [\"-y\", \"@modelcontextprotocol/server-filesystem\", \"/tmp\"]");
        return Ok(());
    }

    println!("{:<20} COMMAND", "NAME");
    println!("{}", "-".repeat(60));
    for srv in &config.mcp.servers {
        let cmd_display = if srv.args.is_empty() {
            srv.command.clone()
        } else {
            format!("{} {}", srv.command, srv.args.join(" "))
        };
        println!("{:<20} {}", srv.name, cmd_display);
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a broad `PermissionSet` suitable for the `mcp serve` operator context.
///
/// `mcp serve` is run directly by the operator to expose AgentOS tools to a
/// local MCP client (e.g. Claude Desktop). The operator has explicitly chosen
/// to expose these tools, so they receive access to all standard tool resource
/// categories. SSRF protection for network resources is enforced by
/// `PermissionSet::is_denied()` regardless of these grants.
fn operator_permissions() -> agentos_types::PermissionSet {
    use agentos_types::{PermissionOp, PermissionSet};
    let mut p = PermissionSet::new();
    // Filesystem — covers fs.user_data, fs.app_logs, fs.system_logs, and all path-based tools.
    p.grant("fs:".into(), true, true, true, None);
    p.grant("fs.user_data".into(), true, true, false, None);
    // Memory subsystem — semantic, episodic, blocks, procedural.
    p.grant("memory.".into(), true, true, false, None);
    // Network URL-style resources (http_client, etc.).
    // SSRF protection for private ranges is enforced by PermissionSet::is_denied() regardless.
    p.grant("net:".into(), true, true, true, None);
    // Network dot-notation resources: network.outbound (web_fetch), network.logs (network_monitor).
    p.grant("network.".into(), true, false, true, None);
    // Hardware HAL queries (hardware.system, hardware.gpu, etc.) — read-only.
    p.grant("hardware.".into(), true, false, false, None);
    // Process tools: process.exec (shell_exec), process.list / process.kill (process_manager).
    p.grant("process.".into(), true, false, true, None);
    // Task queries: task.query (task_status, task_list).
    p.grant("task.".into(), true, false, false, None);
    // Escalation queries: escalation.query uses PermissionOp::Query (not covered by grant()).
    p.grant("escalation.".into(), true, false, false, None);
    p.grant_op("escalation.".into(), PermissionOp::Query, None);
    // User interaction tools: user.notify (notify_user), user.interact (ask_user).
    p.grant("user.".into(), true, true, true, None);
    // Agent registry and messaging: agent.registry (agent_list), agent.message (agent_message).
    p.grant("agent.".into(), true, false, true, None);
    // Data and pipeline tools.
    p.grant("data.".into(), true, true, false, None);
    p
}

// ── McpToolExecutor impl ──────────────────────────────────────────────────────

/// Wraps an AgentOS `ToolRunner` as an `McpToolExecutor` so the kernel's
/// registered tools can be exposed via the MCP server.
struct ToolRunnerExecutor {
    runner: Arc<ToolRunner>,
    data_dir: std::path::PathBuf,
}

#[async_trait]
impl McpToolExecutor for ToolRunnerExecutor {
    async fn list_tools(&self) -> Vec<McpToolDef> {
        self.runner
            .list_tools()
            .into_iter()
            .map(|name| McpToolDef {
                description: format!("AgentOS tool: {}", name),
                input_schema: serde_json::json!({ "type": "object" }),
                name,
            })
            .collect()
    }

    async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        use agentos_tools::traits::ToolExecutionContext;
        use agentos_types::*;
        use tokio_util::sync::CancellationToken;

        let ctx = ToolExecutionContext {
            data_dir: self.data_dir.clone(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            // mcp serve is an operator-invoked local command: grant broad access
            // to all core tool categories.  SSRF protection for network resources
            // is enforced by PermissionSet::is_denied() regardless of these grants.
            permissions: operator_permissions(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: CancellationToken::new(),
        };

        self.runner
            .execute(name, args, ctx)
            .await
            .map_err(|e| e.to_string())
    }
}
