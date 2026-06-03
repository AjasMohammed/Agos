//! Claude MCP Tool Gateway
//!
//! Exposes AgentOS's tool surface to the local `claude` subprocess (driven by
//! the `claude-code` LLM adapter) via a localhost MCP HTTP server. The adapter
//! is configured with `--mcp-config <path>` and allows the 4
//! `mcp__agentos__*` native tools; this module provides the server side.
//!
//! # Security
//!
//! Every MCP `call_tool` invocation runs through [`ToolRunner::execute`] with a
//! fresh [`ToolExecutionContext`] built from the **agent's real**
//! [`PermissionSet`] and capability context — identical to the chat-path
//! context (`kernel.rs:1951`). Capability-token enforcement, path-prefix
//! checks, storage-zone gating, and the capability dispatcher all apply
//! unchanged. The gateway is a protocol bridge, not a security bypass.
//!
//! The HTTP server binds to `127.0.0.1` on an ephemeral port and is protected
//! by a per-agent random bearer token written into the MCP config file (0600).
//! The server runs until the kernel's cancellation token fires (graceful
//! shutdown), so gateways do not outlive the kernel.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agentos_mcp::{build_http_router, McpAuthValidator, McpToolDef, McpToolExecutor};
use agentos_tools::runner::ToolRunner;
use agentos_tools::traits::ToolExecutionContext;
use agentos_types::{
    AgentID, AgentRegistryQuery, AgentRegistrySnapshot, AgentSummary, CapabilityDispatcher,
    CapabilityRegistryQuery, CapabilityRegistrySnapshot, PermissionSet, StorageZoneQuery, TaskID,
    TraceID,
};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::agent_registry::AgentRegistry;
use crate::capability_dispatch::KernelCapabilityDispatcher;
use crate::capability_registry::CapabilityRegistry;
use crate::hooks::HookRegistry;
use crate::kernel::AgentWorkspacePaths;
use crate::managed_storage::ZoneTable;
use agentos_hal::HardwareAbstractionLayer;
use tokio::sync::RwLock;

/// Concrete [`McpToolExecutor`] that routes the 4 MCP meta-tools through the
/// kernel's [`ToolRunner`] using a single agent's real capability context.
///
/// Holds cloned handles to the kernel subsystems it needs — it deliberately
/// does **not** hold an `Arc<Kernel>` (that would create a reference cycle and
/// pull the whole kernel into the detached server task).
pub struct KernelMcpExecutor {
    tool_runner: Arc<ToolRunner>,
    agent_registry: Arc<RwLock<AgentRegistry>>,
    capability_registry: Arc<RwLock<CapabilityRegistry>>,
    capability_dispatcher: Arc<KernelCapabilityDispatcher>,
    hal: Arc<HardwareAbstractionLayer>,
    zone_table: ZoneTable,
    data_dir: PathBuf,
    cancellation_token: CancellationToken,
    /// Kernel's hook registry — fired around every gateway tool call so MCP
    /// tool invocations are audited (`AuditHook`) and gated (`ApprovalHook`)
    /// exactly like chat/task tool calls.
    hook_registry: Arc<HookRegistry>,

    agent_id: AgentID,
    /// The agent's REAL permission set — never broaden.
    permissions: PermissionSet,
    /// Resolved once at construction; the agent's workspace grants are stable
    /// for the lifetime of the connection.
    workspace_paths: AgentWorkspacePaths,
}

impl KernelMcpExecutor {
    /// Construct a new executor for `agent_id` with its real `permissions` and
    /// pre-resolved `workspace_paths`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tool_runner: Arc<ToolRunner>,
        agent_registry: Arc<RwLock<AgentRegistry>>,
        capability_registry: Arc<RwLock<CapabilityRegistry>>,
        capability_dispatcher: Arc<KernelCapabilityDispatcher>,
        hal: Arc<HardwareAbstractionLayer>,
        zone_table: ZoneTable,
        data_dir: PathBuf,
        cancellation_token: CancellationToken,
        hook_registry: Arc<HookRegistry>,
        agent_id: AgentID,
        permissions: PermissionSet,
        workspace_paths: AgentWorkspacePaths,
    ) -> Self {
        Self {
            tool_runner,
            agent_registry,
            capability_registry,
            capability_dispatcher,
            hal,
            zone_table,
            data_dir,
            cancellation_token,
            hook_registry,
            agent_id,
            permissions,
            workspace_paths,
        }
    }

    /// Execute `tool_name` with `payload`, firing the kernel's `ToolPre`/
    /// `ToolPost` hooks around the call so the invocation is audited and
    /// approval-gated like the chat/task paths.
    ///
    /// Fail-closed: if a `ToolPre` hook returns `Abort` (a hard denial, or an
    /// approval-pending escalation), the tool is refused at the gateway. The
    /// async approval-wait dance from `task_executor` is intentionally NOT
    /// implemented here — a denied or pending tool is simply rejected.
    async fn execute_with_hooks(
        &self,
        tool_name: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let task_id = TaskID::new();

        // Fire ToolPre — abort if any hook denies the call.
        let pre_result = self
            .hook_registry
            .fire(&agentos_types::HookEvent::ToolPre {
                task_id,
                agent_id: self.agent_id,
                tool_name: tool_name.to_string(),
                input_json: serde_json::to_string(&payload).unwrap_or_default(),
            })
            .await;
        if let agentos_types::HookResult::Abort(reason) = pre_result {
            return Err(format!("tool '{tool_name}' denied by policy: {reason}"));
        }

        let tool_start = std::time::Instant::now();
        let result = self
            .tool_runner
            .execute(tool_name, payload, self.build_ctx().await)
            .await;
        let duration_ms = tool_start.elapsed().as_millis() as u64;

        // Fire ToolPost — informational, always fires regardless of result.
        let output_json = match &result {
            Ok(v) => serde_json::to_string(v).unwrap_or_default(),
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        };
        self.hook_registry
            .fire(&agentos_types::HookEvent::ToolPost {
                task_id,
                agent_id: self.agent_id,
                tool_name: tool_name.to_string(),
                output_json,
                duration_ms,
            })
            .await;

        result.map_err(|e| e.to_string())
    }

    /// Build a fresh per-call [`ToolExecutionContext`], mirroring the chat path
    /// (`kernel.rs:1951`) exactly so MCP tool calls get identical
    /// agent-scoped capability enforcement.
    async fn build_ctx(&self) -> ToolExecutionContext {
        let agent_snapshot: Arc<dyn AgentRegistryQuery> = {
            let registry = self.agent_registry.read().await;
            let agents: Vec<AgentSummary> = registry
                .list_all()
                .into_iter()
                .map(|p| AgentSummary {
                    id: p.id,
                    name: p.name.clone(),
                    status: format!("{:?}", p.status).to_lowercase(),
                    registered_at: p.created_at,
                })
                .collect();
            Arc::new(AgentRegistrySnapshot::new(agents))
        };

        let capability_registry: Arc<dyn CapabilityRegistryQuery> = {
            let reg = self.capability_registry.read().await;
            Arc::new(CapabilityRegistrySnapshot::new(reg.list_capabilities()))
        };

        ToolExecutionContext {
            data_dir: self.data_dir.clone(),
            task_id: TaskID::new(),
            agent_id: self.agent_id,
            trace_id: TraceID::new(),
            permissions: self.permissions.clone(),
            vault: None,
            hal: Some(self.hal.clone()),
            file_lock_registry: None,
            agent_registry: Some(agent_snapshot),
            task_registry: None,
            escalation_query: None,
            workspace_paths: self.workspace_paths.read.clone(),
            workspace_paths_writable: self.workspace_paths.writable.clone(),
            workspace_paths_executable: self.workspace_paths.executable.clone(),
            capability_registry: Some(capability_registry),
            capability_dispatcher: Some(
                Arc::clone(&self.capability_dispatcher) as Arc<dyn CapabilityDispatcher>
            ),
            storage_zone_query: Some(Arc::new(self.zone_table.clone()) as Arc<dyn StorageZoneQuery>),
            cancellation_token: self.cancellation_token.child_token(),
            tool_categories: None,
        }
    }
}

#[async_trait]
impl McpToolExecutor for KernelMcpExecutor {
    async fn list_tools(&self) -> Vec<McpToolDef> {
        vec![
            McpToolDef {
                name: "search_tools".to_string(),
                description:
                    "Semantic search over AgentOS's tool inventory. Returns tools matching a \
                     natural-language query."
                        .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language description of the capability you need."
                        }
                    },
                    "required": ["query"]
                }),
            },
            McpToolDef {
                name: "describe_tool".to_string(),
                description: "Return the full description, payload schema, and metadata for a \
                              single AgentOS tool by name."
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Exact tool name (e.g. \"file-reader\")."
                        }
                    },
                    "required": ["name"]
                }),
            },
            McpToolDef {
                name: "list_tools".to_string(),
                description: "List the available AgentOS tools, optionally filtered by category \
                              and paginated."
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "category": {
                            "type": "string",
                            "description": "Optional category filter (e.g. \"fs\", \"network\")."
                        },
                        "page": {
                            "type": "integer",
                            "description": "Optional 1-based page number for pagination."
                        }
                    }
                }),
            },
            McpToolDef {
                name: "invoke_tool".to_string(),
                description: "Execute an AgentOS tool by name with a JSON payload. Runs under the \
                              calling agent's real permission set and capability context."
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Exact tool name to invoke."
                        },
                        "payload": {
                            "type": "object",
                            "description": "Tool-specific input payload (JSON object)."
                        }
                    },
                    "required": ["name", "payload"]
                }),
            },
        ]
    }

    async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match name {
            "search_tools" => self.execute_with_hooks("search-tools", args).await,
            "describe_tool" => self.execute_with_hooks("describe-tool", args).await,
            "list_tools" => self.execute_with_hooks("list-tools", args).await,
            "invoke_tool" => {
                let tool_name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "invoke_tool requires a string \"name\" field".to_string())?
                    .to_string();
                let payload = match args.get("payload") {
                    None => serde_json::json!({}),
                    Some(v) if v.is_object() => v.clone(),
                    Some(_) => {
                        return Err("invoke_tool: 'payload' must be a JSON object".into());
                    }
                };
                self.execute_with_hooks(&tool_name, payload).await
            }
            other => Err(format!("Unknown MCP tool: {other}")),
        }
    }
}

/// Bearer-token validator for the per-agent MCP gateway.
///
/// The token is a per-agent random UUID written into the MCP config file. The
/// server binds to localhost only and the token is ephemeral, so a direct
/// `==` comparison is acceptable here (no secret is persisted beyond the
/// process lifetime and there is no remote attacker surface).
struct BearerTokenAuth(String);

#[async_trait]
impl McpAuthValidator for BearerTokenAuth {
    async fn validate_token(&self, token: &str) -> Result<(), String> {
        if token == self.0 {
            Ok(())
        } else {
            Err("invalid MCP bearer token".to_string())
        }
    }
}

/// Handle to a started Claude MCP gateway. The only thing callers need is the
/// path to the generated MCP config file, which is passed to
/// `ClaudeCodeCore::with_mcp_config`.
pub struct ClaudeMcpGateway {
    /// Path to the generated MCP config JSON (`claude-mcp-<agent_id>.json`).
    pub config_path: PathBuf,
}

/// Start a localhost MCP HTTP server backed by `executor`, write the MCP config
/// file under `data_dir`, and return its path.
///
/// The server is spawned with graceful shutdown tied to `cancel`, so all
/// gateway servers stop when the kernel shuts down (pass a child of the
/// kernel's cancellation token).
///
/// Note: a reconnect of the same agent still spawns a fresh server (a bounded
/// leak until kernel shutdown). The per-agent config path is deterministic, so
/// the config file is overwritten rather than accumulated. No per-agent handle
/// registry is maintained — the cancellation-token shutdown is the lifecycle
/// guarantee.
pub async fn start_claude_mcp_gateway(
    executor: Arc<dyn McpToolExecutor>,
    data_dir: &Path,
    agent_id: AgentID,
    cancel: CancellationToken,
) -> anyhow::Result<ClaudeMcpGateway> {
    let token = uuid::Uuid::new_v4().to_string();

    // Bind to an ephemeral localhost port and read back the assigned port.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();

    let auth = Arc::new(BearerTokenAuth(token.clone()));
    let router = build_http_router(executor, auth);

    // Server lives until `cancel` fires (kernel shutdown). No handle is stored.
    tokio::spawn(async move {
        let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
            cancel.cancelled().await;
        });
        if let Err(e) = serve.await {
            tracing::warn!(error = %e, "Claude MCP gateway server exited");
        }
    });

    let config_path = data_dir.join(format!("claude-mcp-{agent_id}.json"));
    let config = serde_json::json!({
        "mcpServers": {
            "agentos": {
                "type": "http",
                "url": format!("http://127.0.0.1:{port}/mcp"),
                "headers": {
                    "Authorization": format!("Bearer {token}")
                }
            }
        }
    });
    // The config file holds a plaintext bearer token; create it 0600 with no
    // 0644 window (atomic create-with-mode on Unix).
    let config_json = serde_json::to_string_pretty(&config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&config_path)
            .map_err(|e| anyhow::anyhow!("write mcp config: {e}"))?;
        std::io::Write::write_all(&mut f, config_json.as_bytes())
            .map_err(|e| anyhow::anyhow!("write mcp config: {e}"))?;
    }
    #[cfg(not(unix))]
    {
        tokio::fs::write(&config_path, config_json.as_bytes()).await?;
    }

    tracing::info!(
        agent_id = %agent_id,
        port,
        config = %config_path.display(),
        "Started Claude MCP tool gateway"
    );

    Ok(ClaudeMcpGateway { config_path })
}
