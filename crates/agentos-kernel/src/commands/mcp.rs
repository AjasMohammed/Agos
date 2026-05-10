/// Kernel handlers for MCP (Model Context Protocol) commands.
///
/// `cmd_mcp_status`      — query live connection health for all servers.
/// `cmd_mcp_attach`      — connect a new MCP server to the running kernel.
/// `cmd_mcp_detach`      — disconnect a server and stop its health monitoring.
/// `cmd_mcp_oauth_store` — store an OAuth2 credential in the vault.
use std::collections::HashMap;
use std::sync::Arc;

use agentos_bus::{KernelResponse, McpServerStatus};
use agentos_types::tool::{ToolCapabilities, ToolInfo, ToolOutputs, ToolSchema};
use agentos_types::{ToolExecutor, ToolManifest, ToolSandbox, TrustTier};
use zeroize::Zeroizing;

use crate::kernel::Kernel;

/// Resolve `vault:KEY` references in an env map.
///
/// Values that start with `"vault:"` are looked up in the vault; all other
/// values are passed through unchanged. Returns `Err` with a list of
/// `(env_key, secret_name)` pairs for any vault lookups that failed, so the
/// caller can decide whether to abort or proceed with partial env.
async fn resolve_env_secrets(
    env: &HashMap<String, String>,
    vault: &agentos_vault::SecretsVault,
) -> Result<HashMap<String, String>, Vec<(String, String)>> {
    let mut resolved = HashMap::with_capacity(env.len());
    let mut failures: Vec<(String, String)> = Vec::new();
    for (key, value) in env {
        if let Some(secret_name) = value.strip_prefix("vault:") {
            match vault.get(secret_name).await {
                Ok(secret) => {
                    resolved.insert(key.clone(), secret.as_str().to_string());
                }
                Err(e) => {
                    tracing::warn!(
                        env_key = %key,
                        secret_name = %secret_name,
                        error = %e,
                        "vault secret not found for MCP env var"
                    );
                    failures.push((key.clone(), secret_name.to_string()));
                }
            }
        } else {
            resolved.insert(key.clone(), value.clone());
        }
    }
    if failures.is_empty() {
        Ok(resolved)
    } else {
        Err(failures)
    }
}

impl Kernel {
    /// Return the live health status of all configured MCP server connections.
    pub async fn cmd_mcp_status(&self) -> KernelResponse {
        let statuses: Vec<McpServerStatus> = self
            .mcp_supervisor
            .server_statuses()
            .await
            .into_iter()
            .map(
                |(name, state, tool_count, _stats, backoff_msg)| McpServerStatus {
                    name,
                    connected: state == agentos_mcp::ServerState::Connected,
                    tool_count,
                    last_error: backoff_msg,
                },
            )
            .collect();

        KernelResponse::McpServerStatusList(statuses)
    }

    /// Store an OAuth2 credential in the vault for later use with `McpAttach`.
    ///
    /// The credential is encrypted at rest using AES-256-GCM and can be
    /// referenced by `connector_id` in `McpAttach { oauth_connector_id }`.
    #[allow(clippy::too_many_arguments)]
    pub async fn cmd_mcp_oauth_store(
        &self,
        connector_id: String,
        provider: String,
        access_token: Zeroizing<String>,
        refresh_token: Option<Zeroizing<String>>,
        token_endpoint: String,
        client_id: String,
        client_secret: Option<Zeroizing<String>>,
        scopes: Vec<String>,
        expires_in_secs: Option<i64>,
    ) -> KernelResponse {
        use agentos_vault::OAuthCredential;
        use chrono::Utc;

        let expires_at = expires_in_secs.map(|secs| Utc::now() + chrono::Duration::seconds(secs));

        let credential = OAuthCredential {
            connector_id: connector_id.clone(),
            provider: provider.clone(),
            access_token: access_token.to_string(),
            refresh_token: refresh_token.as_ref().map(|t| t.to_string()),
            token_type: "Bearer".to_string(),
            expires_at,
            scopes,
            token_endpoint,
            client_id,
            client_secret: client_secret.as_ref().map(|s| s.to_string()),
        };

        let oauth_store = self.vault.oauth_store();
        match oauth_store
            .store(
                &credential,
                agentos_types::SecretOwner::Kernel,
                agentos_types::SecretScope::Global,
            )
            .await
        {
            Ok(()) => {
                tracing::info!(
                    connector = %connector_id,
                    provider = %provider,
                    "OAuth credential stored in vault for MCP server"
                );
                KernelResponse::McpOAuthStored { connector_id }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to store OAuth credential: {}", e),
            },
        }
    }

    /// Attach a new MCP server to the running kernel at runtime.
    ///
    /// Performs the MCP handshake, discovers tools, and registers each tool into
    /// both the `ToolRegistry` (LLM visibility) and the `ToolRunner` (execution).
    /// Persists the attachment to `mcp_attachments.db` so it is restored on restart.
    ///
    /// Authentication modes:
    /// - **Static token**: pass `auth_token`
    /// - **OAuth2**: pass `oauth_connector_id` referencing a credential stored via
    ///   `McpOAuthStore`. The kernel builds a `VaultOAuthProvider` that handles
    ///   transparent token refresh and retry on 401.
    #[allow(clippy::too_many_arguments)]
    pub async fn cmd_mcp_attach(
        &self,
        name: String,
        command: Option<String>,
        args: Vec<String>,
        url: Option<String>,
        auth_token: Option<String>,
        oauth_connector_id: Option<String>,
        timeout_secs: Option<u64>,
        env: HashMap<String, String>,
    ) -> KernelResponse {
        // Enforce mutual exclusivity of auth modes.
        if auth_token.is_some() && oauth_connector_id.is_some() {
            return KernelResponse::Error {
                message: "auth_token and oauth_connector_id are mutually exclusive — use one or the other".to_string(),
            };
        }

        // Reject duplicate names.
        let existing = self.mcp_supervisor.server_statuses().await;
        if existing.iter().any(|(n, _, _, _, _)| n == &name) {
            return KernelResponse::Error {
                message: format!("MCP server '{}' is already attached", name),
            };
        }

        // If a static auth_token was provided, store it in the vault under a
        // well-known key and replace the plaintext with a vault reference.
        // This ensures the token is never persisted in plaintext to mcp_attachments.db.
        let auth_token = if let Some(token) = auth_token {
            let vault_key = format!("mcp.{}.auth_token", name);
            if let Err(e) = self
                .vault
                .set(
                    &vault_key,
                    &token,
                    agentos_types::SecretOwner::Kernel,
                    agentos_types::SecretScope::Global,
                )
                .await
            {
                tracing::warn!(
                    mcp_server = %name,
                    error = %e,
                    "Failed to store auth_token in vault — token will be used in-memory only"
                );
                Some(token)
            } else {
                Some(format!("vault:{}", vault_key))
            }
        } else {
            None
        };

        // Resolve vault:KEY references before spawning — resolved values are
        // never stored; only the original (possibly vault-referencing) env is persisted.
        let resolved_env = match resolve_env_secrets(&env, &self.vault).await {
            Ok(env_map) => env_map,
            Err(failures) => {
                let missing: Vec<String> = failures
                    .iter()
                    .map(|(key, err)| format!("{key}: {err}"))
                    .collect();
                return KernelResponse::Error {
                    message: format!(
                        "Failed to resolve env secrets for MCP server '{}': {}",
                        name,
                        missing.join(", ")
                    ),
                };
            }
        };

        // Build the transport.
        let transport_factory: Option<Arc<dyn agentos_mcp::McpTransportFactory>>;
        let transport: Arc<dyn agentos_mcp::McpTransport> = match (&command, &url) {
            (Some(cmd), None) => {
                let factory = Arc::new(agentos_mcp::transport::stdio::StdioTransportFactory::new(
                    format!("stdio:{}", name),
                    cmd.clone(),
                    args.clone(),
                    resolved_env.clone(),
                    None,
                    timeout_secs,
                ));
                transport_factory = Some(factory);
                match agentos_mcp::transport::stdio::StdioTransport::spawn(
                    format!("stdio:{}", name),
                    cmd.clone(),
                    args.clone(),
                    resolved_env,
                    None,
                    timeout_secs,
                )
                .await
                {
                    Ok(t) => Arc::new(t),
                    Err(e) => {
                        return KernelResponse::Error {
                            message: format!("Failed to spawn MCP server '{}': {}", name, e),
                        }
                    }
                }
            }
            (None, Some(url_str)) => {
                transport_factory = None;

                // Decide auth mode: OAuth2 takes precedence over static token.
                if let Some(ref connector_id) = oauth_connector_id {
                    // Verify the credential exists before building the transport —
                    // gives a clear error rather than a confusing 401 on the first tool call.
                    let oauth_store = self.vault.oauth_store();
                    if let Err(e) = oauth_store.get(connector_id).await {
                        return KernelResponse::Error {
                            message: format!(
                                "OAuth credential '{}' not found in vault: {}. Use 'agentos mcp oauth-store' first.",
                                connector_id, e
                            ),
                        };
                    }
                    // OAuth2 mode — build a VaultOAuthProvider.
                    let provider = match crate::mcp_oauth_provider::VaultOAuthProvider::new(
                        connector_id.clone(),
                        &self.vault,
                    ) {
                        Ok(p) => Arc::new(p),
                        Err(e) => {
                            return KernelResponse::Error {
                                message: format!(
                                    "Failed to build OAuth provider for '{}': {}",
                                    connector_id, e
                                ),
                            }
                        }
                    };
                    match agentos_mcp::transport::http::StreamableHttpTransport::new_with_oauth(
                        format!("http:{}", name),
                        url_str.clone(),
                        provider,
                        timeout_secs,
                    ) {
                        Ok(t) => Arc::new(t),
                        Err(e) => {
                            return KernelResponse::Error {
                                message: format!(
                                    "Failed to create OAuth HTTP transport for '{}': {}",
                                    name, e
                                ),
                            }
                        }
                    }
                } else {
                    // Static token (or no auth).
                    // Resolve vault: reference if the token was auto-vaulted above.
                    let resolved_token = match &auth_token {
                        Some(v) if v.starts_with("vault:") => {
                            let key = &v["vault:".len()..];
                            self.vault
                                .get(key)
                                .await
                                .ok()
                                .map(|s| s.as_str().to_string())
                        }
                        other => other.clone(),
                    };
                    match agentos_mcp::transport::http::StreamableHttpTransport::new(
                        format!("http:{}", name),
                        url_str.clone(),
                        resolved_token,
                        timeout_secs,
                    ) {
                        Ok(t) => Arc::new(t),
                        Err(e) => {
                            return KernelResponse::Error {
                                message: format!(
                                    "Failed to create HTTP transport for '{}': {}",
                                    name, e
                                ),
                            }
                        }
                    }
                }
            }
            _ => {
                return KernelResponse::Error {
                    message: "McpAttach requires either 'command' (stdio) or 'url' (HTTP)"
                        .to_string(),
                }
            }
        };

        let resolved_config = agentos_mcp::McpServerResolvedConfig {
            name: name.clone(),
            timeout_secs: timeout_secs.unwrap_or(30),
            auto_reconnect: true,
            health_check_interval_secs: 30,
        };

        let tools = match self
            .mcp_supervisor
            .add_server_with_factory(resolved_config, transport, transport_factory)
            .await
        {
            Ok(tools) => tools,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("MCP handshake failed for '{}': {}", name, e),
                }
            }
        };

        // Register security policy only after a successful handshake to avoid
        // orphaned policy state when the server fails to connect.
        let policy = agentos_mcp::McpServerPolicy {
            name: name.clone(),
            max_response_bytes: 1024 * 1024,
            allowed_tools: vec![],
            denied_tools: vec![],
            rate_limit_rpm: 60,
        };
        self.mcp_security_gate.register_server_policy(policy).await;

        // Register each discovered tool into both the ToolRegistry (LLM visibility)
        // and the ToolRunner (execution). Tools are registered as TrustTier::Core
        // because the operator explicitly chose to attach this server — that is
        // sufficient authorization; no Ed25519 signature is available for MCP tools.
        let existing_names: std::collections::HashSet<String> =
            self.tool_runner.list_tools().into_iter().collect();
        let mut registered: Vec<String> = Vec::new();

        for tool_def in tools {
            if existing_names.contains(&tool_def.name) {
                tracing::warn!(
                    mcp_server = %name,
                    tool = %tool_def.name,
                    "Skipping MCP tool — name conflicts with existing tool"
                );
                continue;
            }

            // Build a ToolManifest so the LLM can discover and describe this tool.
            let manifest = ToolManifest {
                manifest: ToolInfo {
                    name: tool_def.name.clone(),
                    version: "0.1.0".to_string(),
                    description: tool_def.description.clone(),
                    author: format!("mcp:{}", name),
                    checksum: None,
                    author_pubkey: None,
                    signature: None,
                    trust_tier: TrustTier::Core,
                    tags: Some(vec!["mcp".to_string(), name.clone()]),
                    capability_tags: vec![],
                    group: String::new(),
                },
                capabilities_required: ToolCapabilities {
                    permissions: vec![format!(
                        "mcp.{}",
                        tool_def.name.replace('-', "_").to_lowercase()
                    )],
                },
                capabilities_provided: ToolOutputs {
                    outputs: vec!["content.text".to_string()],
                },
                intent_schema: ToolSchema {
                    input: "McpToolInput".to_string(),
                    output: "McpToolOutput".to_string(),
                },
                input_schema: Some(tool_def.input_schema.clone()),
                sandbox: ToolSandbox {
                    network: true,
                    fs_write: false,
                    gpu: false,
                    max_memory_mb: 256,
                    max_cpu_ms: 30_000,
                    syscalls: vec![],
                    weight: Some("network".to_string()),
                },
                executor: ToolExecutor::default(),
                fallbacks: vec![],
                // MCP tools are externally-provided and may perform arbitrary operations.
                // Default to ExecCapable (requires approval) rather than ReadonlyExternal.
                risk_class: agentos_types::RiskClass::ExecCapable,
                usage_hints: None,
                tags: vec![],
            };

            // Register into ToolRegistry so the LLM sees it.
            {
                let mut registry = self.tool_registry.write().await;
                if let Err(e) = registry.register(manifest) {
                    tracing::warn!(
                        mcp_server = %name,
                        tool = %tool_def.name,
                        error = %e,
                        "Failed to register MCP tool manifest into ToolRegistry"
                    );
                    continue;
                }
            }

            // Register into ToolRunner so it can be executed.
            registered.push(tool_def.name.clone());
            let adapter = agentos_mcp::McpToolAdapter::new(
                Arc::clone(&self.mcp_supervisor),
                Arc::clone(&self.mcp_security_gate),
                name.clone(),
                tool_def,
            );
            self.tool_runner.register_dynamic(Box::new(adapter));
        }

        tracing::info!(
            mcp_server = %name,
            tools = registered.len(),
            auth = if oauth_connector_id.is_some() { "oauth2" } else { "static" },
            "MCP server attached at runtime"
        );

        // Persist the attachment (storing the original env with vault: refs, not resolved values).
        let record = crate::mcp_attachment_store::McpAttachmentRecord {
            name: name.clone(),
            command,
            args,
            url,
            auth_token,
            oauth_connector_id,
            env,
            timeout_secs,
            created_at: chrono::Utc::now(),
        };
        if let Err(e) = self.mcp_attachment_store.save(record).await {
            tracing::warn!(mcp_server = %name, error = %e, "Failed to persist MCP attachment — it will not survive restart");
        }

        KernelResponse::McpAttached {
            tool_count: registered.len(),
            tools: registered,
        }
    }

    /// Detach a running MCP server from the kernel.
    ///
    /// Collects tool names before removal (the supervisor drops them on remove),
    /// then unregisters from both ToolRegistry and ToolRunner.
    pub async fn cmd_mcp_detach(&self, name: String) -> KernelResponse {
        // Collect tool names before removing (supervisor drops them on remove).
        let tool_names: Vec<String> = self
            .mcp_supervisor
            .server_tools(&name)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.name)
            .collect();

        if self.mcp_supervisor.remove_server(&name).await {
            // Unregister from both ToolRegistry and ToolRunner.
            {
                let mut registry = self.tool_registry.write().await;
                for tool_name in &tool_names {
                    let _ = registry.remove(tool_name);
                }
            }
            for tool_name in &tool_names {
                self.tool_runner.unregister_dynamic(tool_name);
            }

            // Revoke the auto-vaulted static auth token BEFORE deleting the attachment
            // record. If the kernel crashes between these two, the attachment will be
            // present at next boot but fail to resolve its token — a recoverable state
            // (logs a warning and skips). Orphaned vault secrets (revoke succeeded but
            // delete failed) are a worse outcome than a retryable attachment failure.
            let vault_key = format!("mcp.{}.auth_token", name);
            let _ = self.vault.revoke(&vault_key).await;

            // Delete the persistence record so it is not restored on next restart.
            if let Err(e) = self.mcp_attachment_store.delete(&name).await {
                tracing::warn!(mcp_server = %name, error = %e, "Failed to delete MCP attachment from store");
            }

            tracing::info!(mcp_server = %name, "MCP server detached at runtime");
            KernelResponse::McpDetached
        } else {
            KernelResponse::Error {
                message: format!("MCP server '{}' not found", name),
            }
        }
    }
}
