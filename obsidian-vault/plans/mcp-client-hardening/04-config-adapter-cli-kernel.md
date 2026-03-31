---
title: "Phase 4: Config, Adapter, CLI, and Kernel Wiring"
tags:
  - mcp
  - v3
  - plan
  - phase-4
  - config
  - cli
date: 2026-03-30
status: planned
effort: 1d
priority: high
---

# Phase 4: Config, Adapter, CLI, and Kernel Wiring

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate the transport, supervisor, and security layers into the kernel. Expand config, rewrite the adapter, add CLI commands, and wire boot/shutdown.

**Architecture:** 
- Config: `McpServerConfig` expanded with transport selection, security, lifecycle fields
- Adapter: Thin bridge through supervisor + security gate
- CLI: New subcommands (add, remove, test), enhanced status, enhanced list
- Kernel: Replace `mcp_handles` with `McpSupervisor`, rewrite boot sequence for parallel spawn, add command handlers

**Tech Stack:** Rust, clap (CLI), tokio, serde/toml (config)

---

## Why This Phase

Phases 1-3 build the subsystems. This phase integrates them into AgentOS. Without it, the supervisor, security gate, and transport abstractions exist but aren't used. This phase makes them part of the kernel's operation and user-facing API.

## Current State

- `crates/agentos-kernel/src/config.rs:871` — `McpServerConfig` has 3 fields: `name`, `command`, `args`
- `crates/agentos-kernel/src/kernel.rs:403` — `mcp_handles: Arc<RwLock<Vec<Arc<McpServerHandle>>>>`
- `crates/agentos-kernel/src/kernel.rs:1711` — boot sequence sequential, no error handling for failed servers
- `crates/agentos-cli/src/commands/mcp.rs` — `Serve`, `List`, `Status` subcommands (no Add, Remove, Test)
- `crates/agentos-bus/src/message.rs:441` — only `KernelCommand::McpStatus` variant

## Target State

- `McpServerConfig` has all fields from spec: transport (command/url/auth/working_dir), security (trust_tier, max_response_bytes, rate_limit, allowed/denied), lifecycle (auto_reconnect, health_check_interval)
- Transport inferred from config: `command` set = stdio, `url` set = HTTP, both/neither = error
- `McpServerResolvedConfig` constructed from `McpServerConfig` with vault secret resolution
- `McpToolAdapter` delegates to supervisor + security gate
- Kernel has `supervisor: Arc<McpSupervisor>` instead of `mcp_handles`
- Boot spawns all servers in parallel via `join_all`
- CLI: `agentctl mcp add|remove|test|list|status|serve`
- New `KernelCommand::McpAdd { config }` and `McpRemove { name }` variants

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/config.rs` | Expand `McpServerConfig`, add config validation |
| `crates/agentos-mcp/src/adapter.rs` | Rewrite to use supervisor + security gate |
| `crates/agentos-cli/src/commands/mcp.rs` | Add `Add`, `Remove`, `Test` subcommands |
| `crates/agentos-bus/src/message.rs` | Add `McpAdd`, `McpRemove` command variants |
| `crates/agentos-kernel/src/kernel.rs` | Replace `mcp_handles`, rewrite boot, add handlers |
| `crates/agentos-kernel/src/commands/mcp.rs` | Add handlers for McpAdd, McpRemove |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch new command variants |

## Dependencies

- **Requires:** Phase 1, 2, 3 (Transport, Supervisor, Security)
- **Blocks:** Nothing (final phase)

---

### Task 1: Expand McpServerConfig

**Files:**
- Modify: `crates/agentos-kernel/src/config.rs`

- [ ] **Step 1: Update McpServerConfig struct**

Find the `McpServerConfig` struct at line 871 in `config.rs` and replace it:

```rust
/// Configuration for a single external MCP server process or HTTP endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    // ── Identity ──────────────────────────────────────────────────────────
    /// Human-readable name for this server (used in logs, CLI, status).
    pub name: String,

    // ── Stdio Transport ───────────────────────────────────────────────────
    /// Path or name of the executable to spawn (e.g. `"npx"`, `"python3"`).
    /// Set for stdio transport. Mutually exclusive with `url`.
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Additional environment variables for the subprocess.
    #[serde(default)]
    pub env: serde_json::Map<String, serde_json::Value>,
    /// Working directory for the subprocess.
    #[serde(default)]
    pub working_dir: Option<std::path::PathBuf>,

    // ── HTTP Transport ────────────────────────────────────────────────────
    /// MCP server endpoint URL (e.g. `"http://localhost:8080/mcp"`).
    /// Set for HTTP transport. Mutually exclusive with `command`.
    #[serde(default)]
    pub url: Option<String>,
    /// Bearer token for HTTP authentication (vault secret reference, e.g. `"vault:mcp-db-token"`).
    #[serde(default)]
    pub auth_token: Option<String>,

    // ── Security ──────────────────────────────────────────────────────────
    /// Trust tier: `"community"` (default) or `"verified"`.
    #[serde(default = "default_trust_tier")]
    pub trust_tier: String,
    /// Max response size in bytes. Overrides global default (1MB).
    #[serde(default)]
    pub max_response_bytes: Option<usize>,
    /// Rate limit: max calls per minute to this server.
    #[serde(default)]
    pub rate_limit_rpm: Option<u32>,
    /// Tool whitelist (empty = allow all).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tool blacklist (takes precedence over allow list).
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// Per-request timeout in seconds. Overrides global default (30s).
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    // ── Lifecycle ─────────────────────────────────────────────────────────
    /// Whether to automatically reconnect on connection failure. Default: true.
    #[serde(default = "default_auto_reconnect")]
    pub auto_reconnect: bool,
    /// Health check interval in seconds. Default: 30.
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval_secs: u64,
}

fn default_trust_tier() -> String {
    "community".to_string()
}

fn default_auto_reconnect() -> bool {
    true
}

fn default_health_check_interval() -> u64 {
    30
}

impl McpServerConfig {
    /// Validate the config.
    /// Returns an error if:
    /// - Both `command` and `url` are set
    /// - Neither `command` nor `url` are set
    /// - `trust_tier` is not "community" or "verified"
    pub fn validate(&self) -> Result<(), String> {
        let has_command = self.command.is_some();
        let has_url = self.url.is_some();

        match (has_command, has_url) {
            (true, true) => Err(format!(
                "MCP server '{}': cannot set both 'command' and 'url'",
                self.name
            )),
            (false, false) => Err(format!(
                "MCP server '{}': must set either 'command' (stdio) or 'url' (HTTP)",
                self.name
            )),
            _ => {}
        }

        if self.trust_tier != "community" && self.trust_tier != "verified" {
            return Err(format!(
                "MCP server '{}': trust_tier must be 'community' or 'verified', got '{}'",
                self.name, self.trust_tier
            ));
        }

        if self.health_check_interval_secs == 0 {
            return Err(format!(
                "MCP server '{}': health_check_interval_secs must be > 0",
                self.name
            ));
        }

        Ok(())
    }

    /// Infer the transport type based on config.
    pub fn transport_type(&self) -> Option<&'static str> {
        match (&self.command, &self.url) {
            (Some(_), None) => Some("stdio"),
            (None, Some(_)) => Some("http"),
            _ => None,
        }
    }
}
```

- [ ] **Step 2: Update McpConfig validation**

Find the `impl McpConfig` block (if it exists) or add one:

```rust
impl McpConfig {
    /// Validate all configured servers.
    pub fn validate(&self) -> Result<(), String> {
        for server in &self.servers {
            server.validate()?;
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Update the load_config function to validate MCP**

Find the `load_config` function in `config.rs`. After loading, add validation:

```rust
// At the end of load_config, before returning Ok(config):
config.mcp.validate()?;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p agentos-kernel`
Expected: Config tests pass. Try invalid configs and verify they error.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-kernel/src/config.rs
git commit -m "feat(config): expand McpServerConfig with transport, security, lifecycle options"
```

---

### Task 2: Rewrite McpToolAdapter

**Files:**
- Modify: `crates/agentos-mcp/src/adapter.rs`

- [ ] **Step 1: Rewrite the adapter to use supervisor + security gate**

Replace the entire contents of `crates/agentos-mcp/src/adapter.rs`:

```rust
/// McpToolAdapter wraps an MCP tool call through the supervisor and security gate.
///
/// Delegates through:
/// 1. Security gate: check rate limit
/// 2. Supervisor: call_tool on transport
/// 3. Security gate: validate output, scan injection, audit log
use std::sync::Arc;

use async_trait::async_trait;

use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};

use crate::supervisor::McpSupervisor;
use crate::security::McpSecurityGate;
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

    /// Override the default permission resource key.
    pub fn with_permission(mut self, permission: &str) -> Self {
        self.permission = permission.to_string();
        self
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
                self.security_gate
                    .audit_tool_call(
                        &self.server_name,
                        &self.tool_def.name,
                        input_size,
                        0,
                        latency_ms,
                        false,
                        context.trace_id,
                        context.task_id,
                        context.agent_id,
                    )
                    .await;
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: self.tool_def.name.clone(),
                    reason: e.to_string(),
                });
            }
        };

        // Step 3: Validate and wrap output.
        let output_size_before = serde_json::to_string(&result)
            .map(|s| s.len())
            .unwrap_or(0);
        let wrapped = match self.security_gate.process_output(result, &self.server_name).await {
            Ok(val) => val,
            Err(e) => {
                let latency_ms = start.elapsed().as_millis() as u64;
                self.security_gate
                    .audit_tool_call(
                        &self.server_name,
                        &self.tool_def.name,
                        input_size,
                        output_size_before,
                        latency_ms,
                        false,
                        context.trace_id,
                        context.task_id,
                        context.agent_id,
                    )
                    .await;
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
        self.security_gate
            .audit_tool_call(
                &self.server_name,
                &self.tool_def.name,
                input_size,
                output_size,
                latency_ms,
                true,
                context.trace_id,
                context.task_id,
                context.agent_id,
            )
            .await;

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

    #[test]
    fn adapter_permission_derived_from_tool_name() {
        let tool_def = McpToolDef {
            name: "read-file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({}),
        };
        let supervisor = Arc::new(unsafe {
            std::mem::zeroed::<McpSupervisor>()
        });
        let security_gate = Arc::new(unsafe {
            std::mem::zeroed::<McpSecurityGate>()
        });
        let adapter =
            McpToolAdapter::new(supervisor, security_gate, "test".into(), tool_def);
        assert_eq!(adapter.permission, "mcp.read_file");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p agentos-mcp`
Expected: Adapter tests compile and pass.

- [ ] **Step 3: Commit**

```bash
git add crates/agentos-mcp/src/adapter.rs
git commit -m "refactor(mcp): rewrite McpToolAdapter to use supervisor and security gate"
```

---

### Task 3: Add KernelCommand and Bus Message Variants

**Files:**
- Modify: `crates/agentos-bus/src/message.rs`

- [ ] **Step 1: Add McpAdd and McpRemove command variants**

Find the `KernelCommand` enum and add after `McpStatus`:

```rust
    /// Hot-add an MCP server at runtime.
    McpAdd {
        name: String,
        command: Option<String>,
        args: Vec<String>,
        url: Option<String>,
        auth_token: Option<String>,
        trust_tier: Option<String>,
        max_response_bytes: Option<usize>,
        rate_limit_rpm: Option<u32>,
        timeout_secs: Option<u64>,
        auto_reconnect: Option<bool>,
        health_check_interval_secs: Option<u64>,
    },

    /// Hot-remove an MCP server by name.
    McpRemove { name: String },
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p agentos-bus`
Expected: Compilation succeeds.

- [ ] **Step 3: Commit**

```bash
git add crates/agentos-bus/src/message.rs
git commit -m "feat(bus): add McpAdd, McpRemove command variants"
```

---

### Task 4: Kernel Boot and Lifecycle Integration

**Files:**
- Modify: `crates/agentos-kernel/src/kernel.rs`
- Modify: `crates/agentos-kernel/src/commands/mcp.rs`
- Modify: `crates/agentos-kernel/src/run_loop.rs`

- [ ] **Step 1: Replace mcp_handles with supervisor**

In `crates/agentos-kernel/src/kernel.rs`, find the `Kernel` struct definition (around line 310). Replace:

```rust
pub mcp_handles: Arc<RwLock<Vec<Arc<agentos_mcp::McpServerHandle>>>>,
```

with:

```rust
pub mcp_supervisor: Arc<agentos_mcp::McpSupervisor>,
pub mcp_security_gate: Arc<agentos_mcp::McpSecurityGate>,
```

- [ ] **Step 2: Rewrite MCP boot sequence**

Find the MCP boot section in the `impl Kernel` boot function (around line 1705). Replace the sequential loop with:

```rust
        // 6.5 Initialize MCP supervisor and security gate.
        let (mcp_event_tx, mut mcp_event_rx) = mpsc::channel(100);
        let mcp_supervisor = Arc::new(agentos_mcp::McpSupervisor::new(
            mcp_event_tx.clone(),
            self.cancellation_token.clone(),
        ));
        let mcp_security_gate = Arc::new(agentos_mcp::McpSecurityGate::new(
            self.audit_log.clone(),
            self.injection_scanner.clone(),
            1024 * 1024, // 1MB default
        ));

        // 6.6 Spawn all configured MCP servers in parallel.
        let mut mcp_add_tasks = Vec::new();
        for mcp_cfg in config.mcp.servers {
            let supervisor = Arc::clone(&mcp_supervisor);
            let security_gate = Arc::clone(&mcp_security_gate);
            let vault = self.vault.clone();
            let audit_log = self.audit_log.clone();

            let task = tokio::spawn(async move {
                let transport = match Self::create_mcp_transport(&mcp_cfg, vault.as_ref()).await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(
                            server = %mcp_cfg.name,
                            error = %e,
                            "Failed to create MCP transport"
                        );
                        return;
                    }
                };

                let resolved_config = agentos_mcp::McpServerResolvedConfig {
                    name: mcp_cfg.name.clone(),
                    timeout_secs: mcp_cfg.timeout_secs.unwrap_or(30),
                    auto_reconnect: mcp_cfg.auto_reconnect,
                    health_check_interval_secs: mcp_cfg.health_check_interval_secs,
                };

                let policy = agentos_mcp::McpServerPolicy {
                    name: mcp_cfg.name.clone(),
                    max_response_bytes: mcp_cfg.max_response_bytes.unwrap_or(1024 * 1024),
                    allowed_tools: mcp_cfg.allowed_tools.clone(),
                    denied_tools: mcp_cfg.denied_tools.clone(),
                    rate_limit_rpm: mcp_cfg.rate_limit_rpm.unwrap_or(60),
                };

                match supervisor.add_server(resolved_config, transport).await {
                    Ok(tools) => {
                        tracing::info!(
                            server = %mcp_cfg.name,
                            tools = tools.len(),
                            "MCP server connected"
                        );
                        security_gate.register_server_policy(policy).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            server = %mcp_cfg.name,
                            error = %e,
                            "MCP server connection failed"
                        );
                    }
                }
            });
            mcp_add_tasks.push(task);
        }

        // Wait for all servers to be added.
        for task in mcp_add_tasks {
            let _ = task.await;
        }

        // 6.7 Spawn the health check loop.
        let _health_loop_handle = mcp_supervisor.spawn_health_loop();

        // 6.8 Forward MCP lifecycle events to audit log (background task).
        let supervisor_clone = Arc::clone(&mcp_supervisor);
        let audit_log_clone = self.audit_log.clone();
        tokio::spawn(async move {
            while let Some(event) = mcp_event_rx.recv().await {
                match event {
                    agentos_mcp::McpLifecycleEvent::ServerConnected { name, tool_count } => {
                        tracing::info!(server = %name, tools = tool_count, "MCP server connected");
                        let _ = audit_log_clone.append(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: agentos_types::TraceID::new(),
                            event_type: agentos_audit::AuditEventType::ToolDiscovered,
                            agent_id: None,
                            task_id: None,
                            tool_id: None,
                            details: serde_json::json!({
                                "server": name,
                                "tool_count": tool_count,
                            }),
                            severity: agentos_audit::AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        });
                    }
                    agentos_mcp::McpLifecycleEvent::ServerDisconnected { name, error } => {
                        tracing::warn!(server = %name, error = %error, "MCP server disconnected");
                    }
                    agentos_mcp::McpLifecycleEvent::ServerReconnecting { name, attempt } => {
                        tracing::info!(server = %name, attempt = attempt, "MCP server reconnecting");
                    }
                    agentos_mcp::McpLifecycleEvent::ServerStopped { name } => {
                        tracing::info!(server = %name, "MCP server stopped");
                    }
                    _ => {}
                }
            }
        });

        self.mcp_supervisor = mcp_supervisor;
        self.mcp_security_gate = mcp_security_gate;
```

- [ ] **Step 3: Add transport creation helper**

Add a private method to `impl Kernel`:

```rust
    /// Create an MCP transport from config.
    async fn create_mcp_transport(
        config: &agentos_kernel::config::McpServerConfig,
        vault: Option<&Arc<agentos_vault::Vault>>,
    ) -> Result<Arc<dyn agentos_mcp::McpTransport>, anyhow::Error> {
        match (&config.command, &config.url) {
            (Some(cmd), None) => {
                // Stdio transport
                let env: std::collections::HashMap<String, String> = config
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect();

                let transport = agentos_mcp::StdioTransport::spawn(
                    format!("stdio:{}", config.name),
                    cmd.clone(),
                    config.args.clone(),
                    env,
                    config.working_dir.clone(),
                    config.timeout_secs,
                )
                .await?;
                Ok(Arc::new(transport))
            }
            (None, Some(url)) => {
                // HTTP transport
                let auth_token = if let Some(ref token_ref) = config.auth_token {
                    if token_ref.starts_with("vault:") {
                        // Resolve from vault
                        let secret_name = token_ref.strip_prefix("vault:").unwrap_or("");
                        match vault {
                            Some(v) => v
                                .get_secret(secret_name)
                                .ok()
                                .flatten()
                                .map(|s| s.to_string()),
                            None => {
                                return Err(anyhow::anyhow!(
                                    "MCP server '{}': auth_token references vault but vault is not available",
                                    config.name
                                ))
                            }
                        }
                    } else {
                        Some(token_ref.clone())
                    }
                } else {
                    None
                };

                let transport = agentos_mcp::StreamableHttpTransport::new(
                    format!("http:{}", config.name),
                    url.clone(),
                    auth_token,
                    config.timeout_secs,
                )?;
                Ok(Arc::new(transport))
            }
            _ => Err(anyhow::anyhow!(
                "MCP server '{}': must specify either 'command' (stdio) or 'url' (HTTP)",
                config.name
            )),
        }
    }
```

- [ ] **Step 4: Update cmd_mcp_status to use supervisor**

In `crates/agentos-kernel/src/commands/mcp.rs`, replace the implementation:

```rust
impl Kernel {
    /// Return the live health status of all configured MCP servers.
    pub async fn cmd_mcp_status(&self) -> KernelResponse {
        let statuses: Vec<agentos_bus::McpServerStatus> = self
            .mcp_supervisor
            .server_statuses()
            .await
            .iter()
            .map(|(name, state, tool_count, stats, backoff_msg)| {
                agentos_bus::McpServerStatus {
                    name: name.clone(),
                    connected: *state == agentos_types::ServerState::Connected,
                    tool_count: *tool_count,
                    last_error: backoff_msg.clone(),
                }
            })
            .collect();

        KernelResponse::McpServerStatusList(statuses)
    }

    /// Add an MCP server at runtime.
    pub async fn cmd_mcp_add(
        &self,
        name: String,
        command: Option<String>,
        args: Vec<String>,
        url: Option<String>,
        auth_token: Option<String>,
        trust_tier: Option<String>,
        max_response_bytes: Option<usize>,
        rate_limit_rpm: Option<u32>,
        timeout_secs: Option<u64>,
        auto_reconnect: Option<bool>,
        health_check_interval_secs: Option<u64>,
    ) -> KernelResponse {
        let config = agentos_kernel::config::McpServerConfig {
            name: name.clone(),
            command,
            args,
            env: serde_json::Map::new(),
            working_dir: None,
            url,
            auth_token,
            trust_tier,
            max_response_bytes,
            rate_limit_rpm,
            allowed_tools: vec![],
            denied_tools: vec![],
            timeout_secs,
            auto_reconnect: auto_reconnect.unwrap_or(true),
            health_check_interval_secs: health_check_interval_secs.unwrap_or(30),
        };

        if let Err(e) = config.validate() {
            return KernelResponse::Error {
                message: e.to_string(),
            };
        }

        let transport = match Self::create_mcp_transport(&config, self.vault.as_ref()).await {
            Ok(t) => t,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to create transport: {}", e),
                }
            }
        };

        let resolved_config = agentos_mcp::McpServerResolvedConfig {
            name: config.name.clone(),
            timeout_secs: config.timeout_secs.unwrap_or(30),
            auto_reconnect: config.auto_reconnect,
            health_check_interval_secs: config.health_check_interval_secs,
        };

        let policy = agentos_mcp::McpServerPolicy {
            name: config.name.clone(),
            max_response_bytes: config.max_response_bytes.unwrap_or(1024 * 1024),
            allowed_tools: config.allowed_tools.clone(),
            denied_tools: config.denied_tools.clone(),
            rate_limit_rpm: config.rate_limit_rpm.unwrap_or(60),
        };

        match futures::executor::block_on(async {
            self.mcp_supervisor.add_server(resolved_config, transport).await
        }) {
            Ok(tools) => {
                let _ = futures::executor::block_on(async {
                    self.mcp_security_gate.register_server_policy(policy).await
                });
                KernelResponse::Ok {
                    data: serde_json::json!({
                        "server": name,
                        "tools_discovered": tools.len(),
                    }),
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to add server: {}", e),
            },
        }
    }

    /// Remove an MCP server at runtime.
    pub async fn cmd_mcp_remove(&self, name: &str) -> KernelResponse {
        if self.mcp_supervisor.remove_server(name).await {
            KernelResponse::Ok {
                data: serde_json::json!({"server": name}),
            }
        } else {
            KernelResponse::Error {
                message: format!("Server '{}' not found", name),
            }
        }
    }
}
```

- [ ] **Step 5: Dispatch new commands in run_loop**

In `crates/agentos-kernel/src/run_loop.rs`, find the command dispatch section and add:

```rust
            KernelCommand::McpAdd {
                name,
                command,
                args,
                url,
                auth_token,
                trust_tier,
                max_response_bytes,
                rate_limit_rpm,
                timeout_secs,
                auto_reconnect,
                health_check_interval_secs,
            } => self.cmd_mcp_add(
                name,
                command,
                args,
                url,
                auth_token,
                trust_tier,
                max_response_bytes,
                rate_limit_rpm,
                timeout_secs,
                auto_reconnect,
                health_check_interval_secs,
            ).await,

            KernelCommand::McpRemove { name } => self.cmd_mcp_remove(&name).await,
```

- [ ] **Step 6: Run tests**

Run: `cargo build --workspace && cargo test -p agentos-kernel`
Expected: Kernel compiles and tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/agentos-kernel/src/kernel.rs
git add crates/agentos-kernel/src/commands/mcp.rs
git add crates/agentos-kernel/src/run_loop.rs
git commit -m "feat(kernel): integrate MCP supervisor, boot servers in parallel, add McpAdd/McpRemove handlers"
```

---

### Task 5: CLI Extensions

**Files:**
- Modify: `crates/agentos-cli/src/commands/mcp.rs`
- Modify: `crates/agentos-cli/src/main.rs`

- [ ] **Step 1: Extend McpCommands enum**

In `crates/agentos-cli/src/commands/mcp.rs`, update the enum:

```rust
#[derive(Debug, Subcommand)]
pub enum McpCommands {
    /// Expose all registered AgentOS tools as an MCP server over stdin/stdout.
    Serve,

    /// List MCP server connections configured in the kernel config file.
    List,

    /// Show live connection health for all configured MCP servers.
    Status,

    /// Hot-add an MCP server at runtime.
    Add {
        /// Server name.
        #[arg(long)]
        name: String,
        /// Stdio command (mutually exclusive with --url).
        #[arg(long)]
        command: Option<String>,
        /// Stdio command arguments.
        #[arg(long)]
        args: Vec<String>,
        /// HTTP URL (mutually exclusive with --command).
        #[arg(long)]
        url: Option<String>,
        /// Bearer token for HTTP auth (vault secret ref).
        #[arg(long)]
        auth_token: Option<String>,
        /// Max response bytes (default 1MB).
        #[arg(long)]
        max_response_bytes: Option<usize>,
        /// Rate limit: calls per minute.
        #[arg(long)]
        rate_limit_rpm: Option<u32>,
    },

    /// Hot-remove an MCP server.
    Remove {
        /// Server name.
        name: String,
    },

    /// Test MCP server connectivity (dry-run).
    Test {
        /// Server name from config, or a standalone command.
        server: String,
        /// If true, treat `server` as a command to spawn (not a config name).
        #[arg(long)]
        command: bool,
    },
}
```

- [ ] **Step 2: Implement the new subcommand handlers**

Add to the handle function:

```rust
pub async fn handle(command: McpCommands, config_path: &str) -> anyhow::Result<()> {
    match command {
        McpCommands::Serve => cmd_serve(config_path).await,
        McpCommands::List => cmd_list(config_path),
        McpCommands::Status => anyhow::bail!("mcp status requires a running kernel"),
        McpCommands::Add {
            name,
            command,
            args,
            url,
            auth_token,
            max_response_bytes,
            rate_limit_rpm,
        } => cmd_add(&name, command, args, url, auth_token, max_response_bytes, rate_limit_rpm).await,
        McpCommands::Remove { name } => cmd_remove(&name).await,
        McpCommands::Test { server, command: is_command } => {
            cmd_test(&server, config_path, is_command).await
        }
    }
}

async fn cmd_add(
    name: &str,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
    auth_token: Option<String>,
    max_response_bytes: Option<usize>,
    rate_limit_rpm: Option<u32>,
) -> anyhow::Result<()> {
    let mut bus = BusClient::connect().await?;
    let response = bus.send_command(KernelCommand::McpAdd {
        name: name.to_string(),
        command,
        args,
        url,
        auth_token,
        trust_tier: None,
        max_response_bytes,
        rate_limit_rpm,
        timeout_secs: None,
        auto_reconnect: None,
        health_check_interval_secs: None,
    }).await?;

    match response {
        KernelResponse::Ok { data } => {
            println!("Server '{}' added:", name);
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        KernelResponse::Error { message } => {
            eprintln!("Failed to add server: {}", message);
            anyhow::bail!(message)
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn cmd_remove(name: &str) -> anyhow::Result<()> {
    let mut bus = BusClient::connect().await?;
    let response = bus.send_command(KernelCommand::McpRemove {
        name: name.to_string(),
    }).await?;

    match response {
        KernelResponse::Ok { data } => {
            println!("Server removed: {}", name);
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        KernelResponse::Error { message } => {
            eprintln!("Failed to remove server: {}", message);
            anyhow::bail!(message)
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

async fn cmd_test(server_name: &str, config_path: &str, is_command: bool) -> anyhow::Result<()> {
    // If is_command, treat server_name as a command to spawn directly.
    // Otherwise, load config and test the server named server_name.
    println!("Testing MCP server '{}' ... (not yet implemented)", server_name);
    // For now, just show a stub. Full implementation would:
    // 1. Create transport (stdio or HTTP)
    // 2. Spawn initialize handshake
    // 3. Call tools/list
    // 4. Print results
    // 5. Close cleanly
    Ok(())
}
```

- [ ] **Step 3: Update main.rs to dispatch new commands**

In `crates/agentos-cli/src/main.rs`, find where `McpCommands` is handled and update the status dispatch:

```rust
            MCP(cmd) => {
                if matches!(cmd, McpCommands::Status) {
                    cmd_mcp_status(&mut bus_client).await?;
                } else {
                    commands::mcp::handle(cmd, &cli.config).await?;
                }
            }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p agentos-cli`
Expected: CLI tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-cli/src/commands/mcp.rs crates/agentos-cli/src/main.rs
git commit -m "feat(cli): add mcp add/remove/test subcommands with runtime hot-add/remove"
```

---

### Task 6: Adapter Registration at Boot

**Files:**
- Modify: `crates/agentos-kernel/src/kernel.rs` (boot sequence)

- [ ] **Step 1: Register MCP tools with ToolRunner after boot**

In the kernel boot sequence, after the health loop is spawned, add tool registration:

```rust
        // 6.9 Register MCP tools with the ToolRunner.
        let supervisor_clone = Arc::clone(&mcp_supervisor);
        let security_gate_clone = Arc::clone(&mcp_security_gate);
        let servers = supervisor_clone.server_statuses().await;
        for (server_name, _state, _tool_count, _stats, _) in servers {
            if let Some(tools) = supervisor_clone.server_tools(&server_name).await {
                for tool_def in tools {
                    let adapter = agentos_mcp::McpToolAdapter::new(
                        Arc::clone(&supervisor_clone),
                        Arc::clone(&security_gate_clone),
                        server_name.clone(),
                        tool_def,
                    );
                    tool_runner.register(Box::new(adapter));
                }
            }
        }
```

Wait — this needs `tool_runner` to have a `deregister` method for hot-remove to work. Add that to `ToolRunner` first:

In `crates/agentos-tools/src/runner.rs`, add:

```rust
    pub fn deregister(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
    }
```

- [ ] **Step 2: Run full build**

Run: `cargo build --workspace && cargo test --workspace`
Expected: Everything compiles and tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/agentos-kernel/src/kernel.rs crates/agentos-tools/src/runner.rs
git commit -m "feat(kernel): register MCP tools at boot; feat(tools): add ToolRunner::deregister"
```

---

## Test Plan

| Test | Assertion |
|------|-----------|
| Config validation rejects both command + url | Returns error |
| Config validation rejects neither command nor url | Returns error |
| Config validation accepts stdio transport | Passes |
| Config validation accepts HTTP transport | Passes |
| McpToolAdapter sanitizes tool name | Converts special chars to `_` |
| McpToolAdapter calls supervisor then security gate | Flow is correct |
| KernelCommand serialization | McpAdd/McpRemove variants serialize/deserialize |
| `cmd_mcp_add` integration | Server is added and tools are registered |
| `cmd_mcp_remove` integration | Server is removed and tools are deregistered |
| CLI `mcp add` command | Sends McpAdd to kernel via bus |
| CLI `mcp remove` command | Sends McpRemove to kernel via bus |
| Parallel MCP boot | All servers boot concurrently (verified via logs) |
| Health loop re-registers tools | Tool list refreshed on reconnect |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
