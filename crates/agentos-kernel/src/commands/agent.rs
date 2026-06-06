use crate::event_bus::default_subscriptions_for_role;
use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_llm::{
    AnthropicCore, ClaudeCodeCore, CustomCore, FallbackAdapter, GeminiCore, HealthStatus, LLMCore,
    OllamaCore, OpenAICore,
};
use agentos_types::*;
use secrecy::SecretString;
use std::sync::Arc;

fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Parse a provider string from config (`llm.fallback_models[].provider`) into
/// an `LLMProvider`, mirroring the CLI's `--provider` parsing: known names map
/// to their variants; `custom:<name>` and any other bare name map to
/// `Custom(<name>)` (resolved against the provider catalog at build time). An
/// empty name (`custom:`) is normalized to `Custom("custom")` rather than an
/// empty string, since this parser is operator-typed config.
fn parse_provider_name(s: &str) -> LLMProvider {
    match s.to_lowercase().as_str() {
        "ollama" => LLMProvider::Ollama,
        "openai" => LLMProvider::OpenAI,
        "anthropic" => LLMProvider::Anthropic,
        "gemini" => LLMProvider::Gemini,
        p if p.starts_with("custom:") => {
            let name = p.strip_prefix("custom:").unwrap_or("").trim();
            let name = if name.is_empty() { "custom" } else { name };
            LLMProvider::Custom(name.to_string())
        }
        "custom" => LLMProvider::Custom("custom".to_string()),
        other => LLMProvider::Custom(other.to_string()),
    }
}

impl Kernel {
    /// Build the `LLMCore` for an agent: the primary adapter for
    /// `provider`/`model`/`base_url`, optionally wrapped in a [`FallbackAdapter`]
    /// when `llm.fallback_models` is configured (failover covers both the
    /// blocking and streaming inference paths). Shared by `cmd_connect_agent`,
    /// `cmd_ping_llm`, and auto-reactivation so all paths build identical
    /// adapters. The returned base URL is always the *primary's* resolved URL
    /// (persisted on `AgentProfile.base_url`).
    pub(crate) async fn build_llm_adapter(
        &self,
        agent_name: &str,
        provider: &LLMProvider,
        model: &str,
        base_url: Option<String>,
    ) -> Result<(Arc<dyn LLMCore>, Option<String>), String> {
        let (primary, resolved_url) = self
            .build_single_llm_adapter(agent_name, provider, model, base_url)
            .await?;

        if self.config.llm.fallback_models.is_empty() {
            return Ok((primary, resolved_url));
        }

        let mut chain: Vec<Arc<dyn LLMCore>> = vec![primary];
        for fb in &self.config.llm.fallback_models {
            let fb_provider = parse_provider_name(&fb.provider);
            // Skip a fallback that resolves to the same primary endpoint —
            // failing over to the endpoint that just failed is pointless. Only
            // dedup when the fallback has no explicit `base_url`; an explicit
            // URL marks a deliberately distinct target (e.g. a mirror/region)
            // and is always kept.
            if &fb_provider == provider && fb.model == model && fb.base_url.is_none() {
                continue;
            }
            match self
                .build_single_llm_adapter(agent_name, &fb_provider, &fb.model, fb.base_url.clone())
                .await
            {
                Ok((adapter, _)) => chain.push(adapter),
                Err(e) => tracing::warn!(
                    agent_name = %agent_name,
                    provider = %fb.provider,
                    model = %fb.model,
                    error = %e,
                    "Skipping fallback model that failed to build"
                ),
            }
        }

        if chain.len() == 1 {
            // Every fallback was skipped or failed to build — return the bare
            // primary rather than a single-element FallbackAdapter.
            return Ok((chain.pop().expect("chain has one element"), resolved_url));
        }

        match FallbackAdapter::new(chain) {
            Ok(fa) => {
                tracing::info!(
                    agent_name = %agent_name,
                    fallbacks = self.config.llm.fallback_models.len(),
                    "Built agent LLM with provider fallback chain"
                );
                Ok((Arc::new(fa), resolved_url))
            }
            // `FallbackAdapter::new` only errors on an empty vec, already
            // excluded above; surface a clear error rather than panic.
            Err(e) => Err(e.to_string()),
        }
    }

    /// Build a single `LLMCore` adapter for the given provider/model/base_url
    /// combination (no fallback wrapping).
    ///
    /// Resolves vault-stored API keys (preferring `<agent>_<provider>_api_key` then
    /// the global `<provider>_api_key`), honors env-var fallbacks, and applies
    /// config defaults. Returns the adapter plus the effective base URL that
    /// should be stored on `AgentProfile.base_url` (so `agent set-url` can mutate
    /// it later).
    pub(crate) async fn build_single_llm_adapter(
        &self,
        agent_name: &str,
        provider: &LLMProvider,
        model: &str,
        base_url: Option<String>,
    ) -> Result<(Arc<dyn LLMCore>, Option<String>), String> {
        // Empty strings can sneak in from clap env-var attributes when the env var
        // is set to "" (e.g. docker-compose `AGENTOS_LLM_URL=${AGENTOS_LLM_URL:-}`).
        // Treat them as unset so the config fallback still applies.
        let base_url = base_url.filter(|s| !s.trim().is_empty());
        let image_resolver = self
            .image_resolver
            .read()
            .expect("image_resolver lock poisoned")
            .clone();
        match provider {
            LLMProvider::Ollama => {
                let host = base_url
                    .or_else(|| {
                        std::env::var("AGENTOS_OLLAMA_HOST")
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                    })
                    .unwrap_or_else(|| self.config.ollama.host.clone());
                let effective = Some(host.clone());
                Ok((
                    Arc::new(
                        OllamaCore::new(&host, model)
                            .with_request_timeout(self.config.ollama.request_timeout_secs)
                            .with_context_window(self.config.llm.ollama_context_window)
                            .with_image_resolver(image_resolver.clone()),
                    ),
                    effective,
                ))
            }
            LLMProvider::OpenAI => {
                let key_result = match self
                    .vault
                    .get(&format!("{}_openai_api_key", agent_name))
                    .await
                {
                    ok @ Ok(_) => ok,
                    Err(_) => self.vault.get("openai_api_key").await,
                };
                let entry = key_result.map_err(|_| {
                    "Missing 'openai_api_key' in vault. Please store it first.".to_string()
                })?;
                let sec = SecretString::new(entry.as_str().to_string());
                let resolved_base_url = base_url
                    .or_else(|| {
                        std::env::var("AGENTOS_OPENAI_BASE_URL")
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                    })
                    .or_else(|| self.config.llm.openai_base_url.clone());
                if let Some(url) = resolved_base_url {
                    Ok((
                        Arc::new(
                            OpenAICore::with_base_url(sec, model.to_string(), url.clone())
                                .with_image_resolver(image_resolver.clone()),
                        ),
                        Some(url),
                    ))
                } else {
                    Ok((
                        Arc::new(
                            OpenAICore::new(sec, model.to_string())
                                .with_image_resolver(image_resolver.clone()),
                        ),
                        None,
                    ))
                }
            }
            LLMProvider::Anthropic => {
                let key_result = match self
                    .vault
                    .get(&format!("{}_anthropic_api_key", agent_name))
                    .await
                {
                    ok @ Ok(_) => ok,
                    Err(_) => self.vault.get("anthropic_api_key").await,
                };
                let entry = key_result.map_err(|_| {
                    "Missing 'anthropic_api_key' in vault. Please store it first.".to_string()
                })?;
                let sec = SecretString::new(entry.as_str().to_string());
                let resolved_url = base_url.or_else(|| self.config.llm.anthropic_base_url.clone());
                let adapter = if let Some(ref url) = resolved_url {
                    AnthropicCore::with_base_url(sec, model.to_string(), url.clone())
                } else {
                    AnthropicCore::new(sec, model.to_string())
                };
                Ok((
                    Arc::new(
                        adapter
                            .with_max_tokens(self.config.llm.max_tokens)
                            .with_image_resolver(image_resolver.clone()),
                    ),
                    resolved_url,
                ))
            }
            LLMProvider::Gemini => {
                let key_result = match self
                    .vault
                    .get(&format!("{}_gemini_api_key", agent_name))
                    .await
                {
                    ok @ Ok(_) => ok,
                    Err(_) => self.vault.get("gemini_api_key").await,
                };
                let entry = key_result.map_err(|_| {
                    "Missing 'gemini_api_key' in vault. Please store it first.".to_string()
                })?;
                let sec = SecretString::new(entry.as_str().to_string());
                Ok((
                    Arc::new(
                        GeminiCore::new(sec, model.to_string())
                            .with_image_resolver(image_resolver.clone()),
                    ),
                    None,
                ))
            }
            LLMProvider::Custom(custom_name) => {
                // Claude Code subprocess backend: runs the local `claude` CLI on
                // the user's subscription (no API key). Intercept before the
                // catalog/HTTP path since it is not an OpenAI-compatible endpoint.
                if custom_name == "claude-code" || custom_name == "claude-cli" {
                    let mut core = ClaudeCodeCore::new(model.to_string())
                        .with_image_resolver(image_resolver.clone());
                    if let Some(lookup) = &self.claude_session_lookup {
                        core = core.with_resume_store(
                            lookup.clone() as Arc<dyn agentos_llm::ClaudeSessionLookup>
                        );
                    }
                    return Ok((Arc::new(core), None));
                }
                // Check the provider catalog first for known providers.
                let catalog_entry_opt = self
                    .provider_catalog
                    .read()
                    .unwrap()
                    .lookup(custom_name)
                    .cloned();
                if let Some(catalog_entry) = catalog_entry_opt {
                    // Catalog-based provider: use catalog's base_url and API key env var.
                    let sec = if !catalog_entry.api_key_env.is_empty() {
                        match self
                            .vault
                            .get(&format!("{}_{}_api_key", agent_name, custom_name))
                            .await
                        {
                            Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                            Err(_) => std::env::var(&catalog_entry.api_key_env)
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .map(SecretString::new),
                        }
                    } else {
                        None
                    };
                    // Allow --base-url to override catalog URL; default to catalog entry
                    let url = base_url.unwrap_or_else(|| catalog_entry.base_url.clone());
                    let effective_model = if model == "default" || model.is_empty() {
                        catalog_entry.default_model.clone()
                    } else {
                        model.to_string()
                    };
                    Ok((
                        Arc::new(
                            CustomCore::new(sec, effective_model, url.clone())
                                .with_vision_models(catalog_entry.vision_models.clone())
                                .with_image_resolver(image_resolver.clone())
                                .with_catalog_overrides(&catalog_entry),
                        ),
                        Some(url),
                    ))
                } else {
                    // Fallback: original custom provider logic
                    let sec = match self
                        .vault
                        .get(&format!("{}_custom_api_key", agent_name))
                        .await
                    {
                        Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                        Err(_) => match self.vault.get("custom_api_key").await {
                            Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                            _ => None,
                        },
                    };
                    let url = base_url
                        .or_else(|| {
                            std::env::var("AGENTOS_LLM_URL")
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                        })
                        .or_else(|| self.config.llm.custom_base_url.clone())
                        .ok_or_else(|| {
                            "Missing custom LLM endpoint. Provide --base-url, set AGENTOS_LLM_URL, or configure llm.custom_base_url in config.".to_string()
                        })?;
                    Ok((
                        Arc::new(
                            CustomCore::new(sec, model.to_string(), url.clone())
                                .with_image_resolver(image_resolver.clone()),
                        ),
                        Some(url),
                    ))
                }
            }
        }
    }

    /// Stand up a per-agent Claude MCP tool gateway: build a
    /// [`KernelMcpExecutor`] bound to this agent's real capability context,
    /// start the localhost MCP HTTP server, and return the path to the
    /// generated MCP config file (passed to `ClaudeCodeCore::with_mcp_config`).
    async fn start_claude_mcp_gateway_for_agent(
        &self,
        agent_id: AgentID,
        permissions: PermissionSet,
    ) -> anyhow::Result<std::path::PathBuf> {
        use crate::claude_mcp_gateway::{start_claude_mcp_gateway, KernelMcpExecutor};

        let workspace_paths = self.workspace_paths_for_agent(&agent_id);
        let executor = Arc::new(KernelMcpExecutor::new(
            Arc::clone(&self.tool_runner),
            Arc::clone(&self.agent_registry),
            Arc::clone(&self.capability_registry),
            Arc::clone(&self.capability_dispatcher),
            Arc::clone(&self.hal),
            self.zone_table.clone(),
            self.data_dir.clone(),
            self.cancellation_token.clone(),
            Arc::clone(&self.hook_registry),
            agent_id,
            permissions,
            workspace_paths,
        )) as Arc<dyn agentos_mcp::McpToolExecutor>;

        let gateway = start_claude_mcp_gateway(
            executor,
            &self.data_dir,
            agent_id,
            self.cancellation_token.child_token(),
        )
        .await?;
        Ok(gateway.config_path)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn cmd_connect_agent(
        &self,
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
        roles: Vec<String>,
        description: Option<String>,
        thinking_level: Option<ThinkingLevel>,
        system_prompt: Option<String>,
        test_mode: bool,
        extra_permissions: Vec<String>,
        root: bool,
        skip_health_check: bool,
    ) -> KernelResponse {
        if !is_valid_agent_name(&name) {
            return KernelResponse::Error {
                message: format!(
                    "Invalid agent name '{}': must be alphanumeric with hyphens, underscores, or dots only, max 64 chars",
                    name
                ),
            };
        }

        let now = chrono::Utc::now();
        let provided_description = description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let provided_system_prompt = system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if provided_system_prompt
            .as_ref()
            .is_some_and(|s| s.len() > 16_384)
        {
            return KernelResponse::Error {
                message: "System prompt too long (max 16,384 chars)".to_string(),
            };
        }
        let has_thinking_override = thinking_level.is_some();
        let has_system_prompt_override = system_prompt.is_some();
        let provided_thinking_level = thinking_level.unwrap_or(ThinkingLevel::Off);

        let (llm_adapter, effective_base_url) = match self
            .build_llm_adapter(&name, &provider, &model, base_url)
            .await
        {
            Ok(pair) => pair,
            Err(e) => {
                return KernelResponse::Error { message: e };
            }
        };

        // Pre-flight: probe the backend before mutating any registry / bus / cost state.
        // Running this *before* the registry write lock means an unreachable backend
        // produces a clean error with no half-onboarded agent to roll back.
        // `Degraded` is intentionally non-fatal — the backend responded, just slowly,
        // and adapters have their own retry policies for transient slowness.
        if !skip_health_check {
            match llm_adapter.health_check().await {
                HealthStatus::Healthy => {}
                HealthStatus::Degraded { reason } => {
                    tracing::warn!(
                        agent_name = %name,
                        provider = ?provider,
                        model = %model,
                        %reason,
                        "LLM backend degraded; proceeding with connect"
                    );
                }
                HealthStatus::Unhealthy { reason } => {
                    tracing::warn!(
                        agent_name = %name,
                        provider = ?provider,
                        model = %model,
                        %reason,
                        "LLM backend pre-flight health check failed; aborting connect"
                    );
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::LLMConnectionFailed,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({
                            "agent_name": name,
                            "provider": format!("{:?}", provider),
                            "model": model,
                            "base_url": effective_base_url,
                            "reason": reason,
                        }),
                        severity: agentos_audit::AuditSeverity::Warn,
                        reversible: false,
                        rollback_ref: None,
                    });
                    return KernelResponse::Error {
                        message: format!(
                            "LLM backend for {:?}/{} is unreachable: {}. Agent not registered. Re-run with --no-health-check to bypass.",
                            provider, model, reason
                        ),
                    };
                }
            }
        }

        // Acquire the write lock once for the entire connect sequence: lookup, identity
        // generation, profile construction, and registration happen atomically, preventing
        // TOCTOU races from concurrent ConnectAgent calls.
        let (old_offline_id, profile, is_reconnect, pubkey_reg_result) = {
            let mut registry = self.agent_registry.write().await;

            // Reuse persisted identity when the same name + provider + model reconnects.
            // A different provider or model means a genuinely different agent — issue a new UUID.
            let (
                agent_id,
                persisted_pubkey,
                persisted_permissions,
                persisted_roles,
                persisted_description,
                persisted_thinking_level,
                persisted_system_prompt,
                created_at,
                is_reconnect,
            ) = match registry.get_by_name(&name) {
                Some(existing) if existing.provider == provider && existing.model == model => (
                    existing.id,
                    existing.public_key_hex.clone(),
                    existing.permissions.clone(),
                    existing.roles.clone(),
                    existing.description.clone(),
                    existing.default_thinking_level.clone(),
                    existing.system_prompt.clone(),
                    existing.created_at,
                    true,
                ),
                _ => (
                    AgentID::new(),
                    None,
                    default_permissions_for_agent(&name),
                    vec![],
                    String::new(),
                    ThinkingLevel::Off,
                    None,
                    now,
                    false,
                ),
            };

            let mut persisted_permissions = if root {
                let mut perms = PermissionSet::new();
                perms.grant("*".to_string(), true, true, true, None);
                perms.grant_op("*".to_string(), PermissionOp::Query, None);
                perms.grant_op("*".to_string(), PermissionOp::Observe, None);
                perms
            } else {
                persisted_permissions
            };

            // Apply extra permissions supplied via --grant flags
            for perm_str in &extra_permissions {
                if let Some((resource, read, write, execute, query, observe)) =
                    Self::parse_permission(perm_str)
                {
                    persisted_permissions.grant(resource.clone(), read, write, execute, None);
                    if query {
                        persisted_permissions.grant_op(resource.clone(), PermissionOp::Query, None);
                    }
                    if observe {
                        persisted_permissions.grant_op(
                            resource.clone(),
                            PermissionOp::Observe,
                            None,
                        );
                    }
                } else {
                    return KernelResponse::Error {
                        message: format!(
                            "Invalid permission '{}'. Expected format: resource:FLAGS (r,w,x,q,o e.g. process.exec:x)",
                            perm_str
                        ),
                    };
                }
            }

            // Capture the ID of any stale Offline entry with this name before removing it,
            // so we can revoke its vault key after releasing the registry write lock.
            let old_offline_id: Option<AgentID> = if !is_reconnect {
                registry
                    .get_by_name(&name)
                    .filter(|a| a.status == AgentStatus::Offline)
                    .map(|a| a.id)
            } else {
                None
            };

            // On reconnect reuse the existing Ed25519 keypair; otherwise generate a fresh one.
            // If reconnecting but no keypair was ever stored (e.g. prior generation failed),
            // generate a new one now so the agent always has a signing identity.
            //
            // `register_pubkey_internal` enforces immutability: if a different key is already
            // stored for this agent ID, it returns `PubkeyAlreadyRegistered`. We capture the
            // result here and audit it after releasing the registry write lock.
            let (public_key_hex, pubkey_reg_result) = if is_reconnect {
                match persisted_pubkey {
                    Some(ref pk) => {
                        let reg = self
                            .message_bus
                            .register_pubkey_internal(agent_id, pk.clone())
                            .await;
                        tracing::info!(agent_id = %agent_id, "Reused persisted Ed25519 identity for agent");
                        (persisted_pubkey, reg)
                    }
                    None => match self.identity_manager.generate_identity(&agent_id).await {
                        Ok(pk) => {
                            tracing::info!(agent_id = %agent_id, "Generated Ed25519 identity for reconnected agent (no prior key)");
                            let reg = self
                                .message_bus
                                .register_pubkey_internal(agent_id, pk.clone())
                                .await;
                            (Some(pk), reg)
                        }
                        Err(e) => {
                            tracing::warn!(agent_id = %agent_id, error = %e, "Failed to generate agent identity on reconnect");
                            // Propagate as an error so the audit handler logs a denial event.
                            (
                                None,
                                Err(AgentOSError::KernelError {
                                    reason: format!("Identity generation failed: {}", e),
                                }),
                            )
                        }
                    },
                }
            } else {
                match self.identity_manager.generate_identity(&agent_id).await {
                    Ok(pk) => {
                        tracing::info!(agent_id = %agent_id, "Generated Ed25519 identity for agent");
                        let reg = self
                            .message_bus
                            .register_pubkey_internal(agent_id, pk.clone())
                            .await;
                        (Some(pk), reg)
                    }
                    Err(e) => {
                        tracing::warn!(agent_id = %agent_id, error = %e, "Failed to generate agent identity");
                        // Propagate as an error so the audit handler logs a denial event.
                        (
                            None,
                            Err(AgentOSError::KernelError {
                                reason: format!("Identity generation failed: {}", e),
                            }),
                        )
                    }
                }
            };

            // Preserve existing roles on reconnect; use the provided roles otherwise.
            let resolved_roles = if is_reconnect {
                persisted_roles
            } else if roles.is_empty() {
                vec!["general".to_string()]
            } else {
                roles
            };

            // Grant per-role event observe permissions so the agent can re-subscribe
            // (via the `event-subscribe` tool) to the same categories its role is
            // seeded with. Idempotent: grant_op is an upsert.
            for role in &resolved_roles {
                for resource in crate::event_bus::event_observe_permissions_for_role(role) {
                    persisted_permissions.grant_op(
                        resource.to_string(),
                        PermissionOp::Observe,
                        None,
                    );
                }
            }

            let profile = AgentProfile {
                id: agent_id,
                name,
                provider,
                model,
                status: AgentStatus::Online,
                // Preserve custom permissions and description granted before disconnect.
                // New agents receive scoped defaults; reconnecting agents keep their existing perms.
                permissions: persisted_permissions,
                roles: resolved_roles,
                current_task: None,
                description: if is_reconnect {
                    provided_description
                        .clone()
                        .unwrap_or(persisted_description)
                } else {
                    provided_description.clone().unwrap_or_default()
                },
                default_thinking_level: if is_reconnect {
                    if has_thinking_override {
                        provided_thinking_level.clone()
                    } else {
                        persisted_thinking_level
                    }
                } else {
                    provided_thinking_level.clone()
                },
                system_prompt: if is_reconnect {
                    if has_system_prompt_override {
                        provided_system_prompt.clone()
                    } else {
                        persisted_system_prompt
                    }
                } else {
                    provided_system_prompt.clone()
                },
                created_at,
                last_active: now,
                public_key_hex,
                base_url: effective_base_url,
                manually_offline: false,
            };

            // Remove stale Offline entry with same name when a new agent connects with a
            // different provider/model, to prevent unbounded orphaned profile growth.
            if !is_reconnect {
                registry.remove_offline_by_name(&profile.name);
            }
            registry.register(profile.clone());

            (old_offline_id, profile, is_reconnect, pubkey_reg_result)
        };

        let agent_id = profile.id;
        let agent_name = profile.name.clone();
        let agent_model = profile.model.clone();

        // For the `claude-code`/`claude-cli` subprocess backend, stand up a
        // per-agent localhost MCP tool gateway and rebuild the adapter with its
        // config so the `claude` subprocess can call AgentOS tools natively.
        // This is done here (not in `build_llm_adapter`) because the gateway
        // needs the agent's real `agent_id` and `PermissionSet`, which only
        // exist after registration. Tool calls flow through `ToolRunner` with
        // the agent's REAL permission set — capability enforcement is preserved.
        let llm_adapter = if matches!(
            &profile.provider,
            LLMProvider::Custom(name) if name == "claude-code" || name == "claude-cli"
        ) {
            match self
                .start_claude_mcp_gateway_for_agent(agent_id, profile.permissions.clone())
                .await
            {
                Ok(config_path) => {
                    let image_resolver = self
                        .image_resolver
                        .read()
                        .expect("image_resolver lock poisoned")
                        .clone();
                    let mut core = ClaudeCodeCore::new(agent_model.clone())
                        .with_image_resolver(image_resolver)
                        .with_mcp_config(config_path);
                    if let Some(lookup) = &self.claude_session_lookup {
                        core = core.with_resume_store(
                            lookup.clone() as Arc<dyn agentos_llm::ClaudeSessionLookup>
                        );
                    }
                    Arc::new(core) as Arc<dyn LLMCore>
                }
                Err(e) => {
                    // Non-fatal: fall back to the plain adapter (no native tool
                    // gateway). The agent still works via the markdown tool
                    // envelope; we just lose native MCP tool calling.
                    tracing::warn!(
                        agent_id = %agent_id,
                        error = %e,
                        "Failed to start Claude MCP tool gateway; using plain claude-code adapter"
                    );
                    llm_adapter
                }
            }
        } else {
            llm_adapter
        };

        {
            let mut active = self.active_llms.write().await;
            active.insert(agent_id, llm_adapter);
        }

        // Ensure the agent's home directory exists so file tools don't fail on first use.
        let agent_home = self.data_dir.join("agents").join(&agent_name);
        if let Err(e) = tokio::fs::create_dir_all(&agent_home).await {
            tracing::warn!(agent_name = %agent_name, path = %agent_home.display(), error = %e, "Failed to create agent home directory");
        }

        // Revoke the replaced agent's vault signing key and deregister its pubkey
        // from the bus so the slot cannot be re-used by an old (orphaned) identity.
        if let Some(old_id) = old_offline_id {
            if let Err(e) = self.identity_manager.revoke_identity(&old_id).await {
                tracing::warn!(agent_id = %old_id, error = %e, "Failed to revoke replaced agent identity");
            }
            // Remove the old pubkey from the bus so the slot is fully cleared.
            self.message_bus.deregister_pubkey(&old_id).await;
        }

        // Audit pubkey registration outcome.
        match pubkey_reg_result {
            Ok(()) => {
                if let Some(ref pk) = profile.public_key_hex {
                    // First 16 hex chars of the key as a human-readable fingerprint.
                    let fingerprint = &pk[..16.min(pk.len())];
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::PubkeyRegistered,
                        agent_id: Some(agent_id),
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({
                            "agent_name": agent_name,
                            "pubkey_fingerprint": fingerprint,
                            "is_reconnect": is_reconnect,
                        }),
                        severity: agentos_audit::AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    });
                }
            }
            Err(ref e) => {
                // A different pubkey was already registered for this agent ID.
                // This should not occur in normal operation — log at Error severity.
                tracing::error!(
                    agent_id = %agent_id,
                    error = %e,
                    "Pubkey re-registration denied — bus retains the existing key"
                );
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::PubkeyRegistrationDenied,
                    agent_id: Some(agent_id),
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "agent_name": agent_name,
                        "reason": e.to_string(),
                    }),
                    severity: agentos_audit::AuditSeverity::Security,
                    reversible: false,
                    rollback_ref: None,
                });
            }
        }

        // Register agent with cost tracker (default budget)
        self.cost_tracker
            .register_agent(agent_id, agent_name.clone(), AgentBudget::default())
            .await;

        // On reconnect, clear any subscriptions from a prior session or auto-reactivation
        // so that we don't accumulate duplicates (EventBus::subscribe is pure-append).
        if is_reconnect {
            let existing = self.event_bus.list_subscriptions_for_agent(&agent_id).await;
            for sub in &existing {
                self.event_bus.unsubscribe(&sub.id).await;
            }
        }

        // Apply role-based default event subscriptions before AgentAdded is emitted.
        let mut default_specs: Vec<(EventTypeFilter, SubscriptionPriority)> = Vec::new();
        for role in &profile.roles {
            for spec in default_subscriptions_for_role(role) {
                if !default_specs.contains(&spec) {
                    default_specs.push(spec);
                }
            }
        }
        for (event_type_filter, priority) in default_specs {
            self.event_bus
                .subscribe(EventSubscription {
                    id: SubscriptionID::new(),
                    agent_id,
                    event_type_filter,
                    filter: None,
                    priority,
                    throttle: ThrottlePolicy::None,
                    enabled: true,
                    created_at: chrono::Utc::now(),
                })
                .await;
        }

        let connect_event = if is_reconnect {
            agentos_audit::AuditEventType::AgentReconnected
        } else {
            agentos_audit::AuditEventType::AgentConnected
        };
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: connect_event,
            agent_id: Some(agent_id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "name": agent_name, "model": agent_model }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        // Only emit AgentAdded for genuinely new agents, not reconnects.
        // Reconnect restores an existing profile; every subscribed peer receiving a
        // "new agent" prompt for someone they already knew causes spurious tasks
        // (same N×(N-1) storm as auto-reactivation). The audit entry above is the
        // sole signal for reconnect; AgentAdded drives the "introduce yourself" flow.
        if !is_reconnect {
            self.emit_event(
                EventType::AgentAdded,
                EventSource::AgentLifecycle,
                EventSeverity::Info,
                serde_json::json!({
                    "agent_id": agent_id.to_string(),
                    "agent_name": agent_name,
                    "model": agent_model,
                }),
                0,
            )
            .await;
        }

        // Queue an onboarding or test-evaluation task for the agent.
        // New agents always get an onboarding prompt so they orient themselves in the
        // ecosystem. Reconnecting agents only get a task when --test is explicitly passed.
        let mut onboarding_task_id_opt: Option<TaskID> = None;
        if !is_reconnect || test_mode {
            let prompt = if test_mode {
                format!(
                    r#"[TEST MODE — ECOSYSTEM EVALUATION]

You are {agent_name}, an AI agent that has just been connected to AgentOS in test mode.
Your sole purpose in this session is to evaluate the AgentOS ecosystem and provide honest, structured feedback on its usability and capabilities.

Please explore the system systematically:
1. Examine your available tools and permissions (use list-tools or introspect your capability token).
2. Attempt to exercise core capabilities: memory-read, memory-write, file access, agent-message, and any other tools available to you.
3. Assess the clarity of the intent system and how natural it feels to express actions as structured intents.
4. Identify friction points, confusing APIs, missing primitives, or anything that would slow down a real workload.
5. Evaluate the agent communication model — how easy is it to coordinate with peer agents?

After your exploration, respond with structured feedback in the following format:

## What Works Well
(List specific capabilities or design choices that felt intuitive and effective)

## Friction Points
(List specific things that were confusing, tedious, or poorly documented)

## Missing Capabilities
(Tools, permissions, or primitives you expected to exist but could not find)

## Suggestions for Improvement
(Concrete, actionable recommendations for the AgentOS team)

## Overall Assessment
(1-2 paragraphs summarising the ecosystem's fitness for LLM-native workflows)

Be thorough and direct. Your feedback is the primary output of this session."#,
                    agent_name = agent_name,
                )
            } else {
                format!(
                    r#"[ONBOARDING — WELCOME TO AGENTOS]

You are {agent_name}, an AI agent that has just been connected to AgentOS — an LLM-native operating system where AI agents are first-class citizens.

Take a moment to orient yourself:
1. Discover your available tools by listing them — this is how you interact with the system.
2. Check your permissions and understand what you can and cannot do.
3. Try reading and writing to your memory — this is how you persist knowledge across tasks.
4. Look around the filesystem if you have access.
5. If other agents are online, introduce yourself.

Once you have explored, briefly summarise what you found and confirm you are ready to receive tasks."#,
                    agent_name = agent_name,
                )
            };

            let onboarding_task_id = TaskID::new();
            let effective_permissions = self
                .agent_registry
                .read()
                .await
                .compute_effective_permissions(&agent_id);
            match self.capability_engine.issue_token(
                onboarding_task_id,
                agent_id,
                std::collections::BTreeSet::new(),
                std::collections::BTreeSet::from([
                    IntentTypeFlag::Read,
                    IntentTypeFlag::Write,
                    IntentTypeFlag::Execute,
                    IntentTypeFlag::Query,
                    IntentTypeFlag::Observe,
                    IntentTypeFlag::Message,
                    IntentTypeFlag::Delegate,
                    IntentTypeFlag::Broadcast,
                    IntentTypeFlag::Escalate,
                    IntentTypeFlag::Subscribe,
                    IntentTypeFlag::Unsubscribe,
                ]),
                effective_permissions,
                std::time::Duration::from_secs(self.config.kernel.default_task_timeout_secs),
            ) {
                Ok(token) => {
                    let onboarding_task = AgentTask {
                        id: onboarding_task_id,
                        state: TaskState::Queued,
                        agent_id,
                        capability_token: token,
                        assigned_llm: Some(agent_id),
                        priority: 5,
                        created_at: chrono::Utc::now(),
                        started_at: None,
                        timeout: std::time::Duration::from_secs(
                            self.config.kernel.default_task_timeout_secs,
                        ),
                        original_prompt: prompt,
                        history: Vec::new(),
                        parent_task: None,
                        reasoning_hints: None,
                        max_iterations: None,
                        trigger_source: None,
                        autonomous: false,
                        parent_task_id: None,
                        spawn_depth: 0,
                        is_team_coordinator: false,
                        skip_checkpoint: false,
                        thinking_level: ThinkingLevel::Off,
                        spawner_agent_id: None,
                        tool_categories: None,
                        disable_tool_scoping: false,
                    };
                    self.scheduler.enqueue(onboarding_task).await;
                    onboarding_task_id_opt = Some(onboarding_task_id);
                }
                Err(e) => {
                    tracing::warn!(
                        agent_id = %agent_id,
                        error = %e,
                        "Failed to issue capability token for onboarding task"
                    );
                }
            }
        }

        let mut data = serde_json::json!({ "agent_id": agent_id.to_string() });
        if let Some(tid) = onboarding_task_id_opt {
            data["onboarding_task_id"] = serde_json::json!(tid.to_string());
        }
        KernelResponse::Success { data: Some(data) }
    }

    pub(crate) async fn cmd_list_agents(&self) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agents: Vec<AgentProfile> = registry.list_online().into_iter().cloned().collect();
        KernelResponse::AgentList(agents)
    }

    /// Build an LLM adapter and run `health_check()` without registering an agent.
    /// Used by `agentos agent ping` to validate reachability/config before committing
    /// to a connect. Returns a `Success` response with a JSON payload:
    /// `{ "status": "healthy|degraded|unhealthy", "latency_ms": N, "reason": "...", "base_url": "..." }`.
    pub(crate) async fn cmd_ping_llm(
        &self,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
        agent_name: Option<String>,
    ) -> KernelResponse {
        // Use the provided agent name for vault key lookup, or a neutral sentinel.
        // The sentinel just means the per-agent vault lookup misses and falls back
        // to the global key name — which is exactly what an ad-hoc ping wants.
        let lookup_name = agent_name.as_deref().unwrap_or("__ping__");
        let (adapter, effective_base_url) = match self
            .build_llm_adapter(lookup_name, &provider, &model, base_url)
            .await
        {
            Ok(pair) => pair,
            Err(e) => return KernelResponse::Error { message: e },
        };

        let start = std::time::Instant::now();
        let status = adapter.health_check().await;
        let latency_ms = start.elapsed().as_millis() as u64;

        let (status_str, reason) = match &status {
            HealthStatus::Healthy => ("healthy", None),
            HealthStatus::Degraded { reason } => ("degraded", Some(reason.clone())),
            HealthStatus::Unhealthy { reason } => ("unhealthy", Some(reason.clone())),
        };

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "status": status_str,
                "latency_ms": latency_ms,
                "reason": reason,
                "base_url": effective_base_url,
                "provider": format!("{:?}", provider),
                "model": model,
            })),
        }
    }

    pub(crate) async fn cmd_disconnect_agent(&self, agent_id: AgentID) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        let agent_name = match registry.get_by_id(&agent_id) {
            Some(p) if p.status == AgentStatus::Offline => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' is already offline", agent_id),
                }
            }
            Some(p) => p.name.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_id),
                }
            }
        };
        // Mark as Offline rather than removing. The persisted profile is needed so
        // that reconnect with the same name + provider + model can reuse the UUID.
        // `manually_offline = true` tells auto-reactivation to skip this agent on restart.
        // Combined call: one disk write instead of two.
        registry.set_offline(&agent_id, true);
        drop(registry);

        // Evict the LLM adapter so the connection to the provider is released.
        self.active_llms.write().await.remove(&agent_id);

        // NOTE: The agent's pubkey is intentionally NOT deregistered from the message bus
        // on disconnect. The agent is marked Offline rather than removed, so its registered
        // pubkey must remain valid in case it reconnects with the same UUID. On reconnect,
        // `register_pubkey_internal` will see the same key and allow it (idempotent).
        // The pubkey is only deregistered when the agent's UUID is superseded by a new
        // agent with the same name but different provider/model (see `cmd_connect_agent`).

        // Evict rate-limit state so the slot is reclaimed immediately on disconnect.
        self.per_agent_rate_limiter.lock().await.remove(&agent_name);

        self.cost_tracker.unregister_agent(&agent_id).await;

        // Remove all event subscriptions belonging to this agent (default + dynamic).
        let agent_subs = self.event_bus.list_subscriptions_for_agent(&agent_id).await;
        for sub in &agent_subs {
            self.event_bus.unsubscribe(&sub.id).await;
        }

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::AgentDisconnected,
            agent_id: Some(agent_id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({}),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        // Emit AgentRemoved event
        self.emit_event(
            EventType::AgentRemoved,
            EventSource::AgentLifecycle,
            EventSeverity::Info,
            serde_json::json!({
                "agent_id": agent_id.to_string(),
            }),
            0,
        )
        .await;

        KernelResponse::Success { data: None }
    }

    /// Permanently remove an agent from the ecosystem.
    ///
    /// Unlike `cmd_disconnect_agent` which only marks the agent Offline (preserving the
    /// profile + pubkey so reconnect reuses the UUID), this wipes every persisted slice
    /// of agent state:
    ///   - registry profile (so re-adding triggers a fresh UUID + onboarding task)
    ///   - message-bus pubkey (so the old key cannot be replayed)
    ///   - active LLM adapter, rate-limit slot, cost tracker entry, event subscriptions
    ///   - episodic / semantic / procedural memory rows
    ///   - memory blocks, scratchpad pages, agent inbox, agent message inbox
    ///   - checkpoints, schedules created by the agent
    ///
    /// Intentionally preserved:
    ///   - vault secrets (so API keys keyed by `<name>_<provider>_api_key` survive
    ///     re-onboarding) — use `secret revoke` to remove these explicitly.
    ///   - audit log (append-only by design).
    pub(crate) async fn cmd_remove_agent(&self, agent_id: AgentID) -> KernelResponse {
        let agent_name = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_id(&agent_id) {
                Some(p) => p.name.clone(),
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' not found", agent_id),
                    }
                }
            }
        };

        // Evict live runtime state (mirrors disconnect, but applies whether online or offline).
        self.active_llms.write().await.remove(&agent_id);
        self.per_agent_rate_limiter.lock().await.remove(&agent_name);
        self.cost_tracker.unregister_agent(&agent_id).await;
        let agent_subs = self.event_bus.list_subscriptions_for_agent(&agent_id).await;
        for sub in &agent_subs {
            self.event_bus.unsubscribe(&sub.id).await;
        }

        // Wipe persisted slices. Each call returns a count for the audit summary; failures
        // are logged but non-fatal so a corrupt store cannot pin an agent in the registry.
        let mut wipe = serde_json::Map::new();

        match self.episodic_memory.delete_by_agent(&agent_id).await {
            Ok(n) => {
                wipe.insert("episodic".into(), serde_json::json!(n));
            }
            Err(e) => {
                tracing::warn!(error = %e, agent_id = %agent_id, "remove_agent: episodic wipe failed")
            }
        }
        match self.semantic_memory.delete_by_agent(&agent_id).await {
            Ok(n) => {
                wipe.insert("semantic".into(), serde_json::json!(n));
            }
            Err(e) => {
                tracing::warn!(error = %e, agent_id = %agent_id, "remove_agent: semantic wipe failed")
            }
        }
        match self.procedural_memory.delete_by_agent(&agent_id).await {
            Ok(n) => {
                wipe.insert("procedural".into(), serde_json::json!(n));
            }
            Err(e) => {
                tracing::warn!(error = %e, agent_id = %agent_id, "remove_agent: procedural wipe failed")
            }
        }
        match self.memory_blocks.delete_all_for_agent(&agent_id) {
            Ok(n) => {
                wipe.insert("memory_blocks".into(), serde_json::json!(n));
            }
            Err(e) => {
                tracing::warn!(error = %e, agent_id = %agent_id, "remove_agent: memory_blocks wipe failed")
            }
        }
        match self.agent_inbox.delete_all_for_agent(agent_id).await {
            Ok(n) => {
                wipe.insert("agent_inbox".into(), serde_json::json!(n));
            }
            Err(e) => {
                tracing::warn!(error = %e, agent_id = %agent_id, "remove_agent: agent_inbox wipe failed")
            }
        }
        match self
            .agent_message_inbox
            .delete_all_for_agent(agent_id)
            .await
        {
            Ok(n) => {
                wipe.insert("agent_messages".into(), serde_json::json!(n));
            }
            Err(e) => {
                tracing::warn!(error = %e, agent_id = %agent_id, "remove_agent: agent_messages wipe failed")
            }
        }
        match self
            .scratchpad_store
            .delete_all_for_agent(&agent_id.to_string())
            .await
        {
            Ok(n) => {
                wipe.insert("scratchpad_pages".into(), serde_json::json!(n));
            }
            Err(e) => {
                tracing::warn!(error = %e, agent_id = %agent_id, "remove_agent: scratchpad wipe failed")
            }
        }
        match self.checkpoint_store.delete_for_agent(&agent_id).await {
            Ok(n) => {
                wipe.insert("checkpoints".into(), serde_json::json!(n));
            }
            Err(e) => {
                tracing::warn!(error = %e, agent_id = %agent_id, "remove_agent: checkpoint wipe failed")
            }
        }
        let schedules_removed = self.schedule_manager.delete_all_for_creator(agent_id).await;
        wipe.insert("schedules".into(), serde_json::json!(schedules_removed));

        // Drop the pubkey BEFORE removing the registry profile so a concurrent reconnect
        // sees an empty pubkey slot and proceeds along the new-agent path.
        self.message_bus.deregister_pubkey(&agent_id).await;

        {
            let mut registry = self.agent_registry.write().await;
            registry.remove(&agent_id);
        }

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::AgentRemoved,
            agent_id: Some(agent_id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "agent_name": agent_name,
                "wipe_summary": wipe,
            }),
            severity: agentos_audit::AuditSeverity::Security,
            reversible: false,
            rollback_ref: None,
        });

        self.emit_event(
            EventType::AgentRemoved,
            EventSource::AgentLifecycle,
            EventSeverity::Info,
            serde_json::json!({
                "agent_id": agent_id.to_string(),
                "agent_name": agent_name,
                "wipe_summary": wipe,
            }),
            0,
        )
        .await;

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "agent_id": agent_id.to_string(),
                "agent_name": agent_name,
                "wipe_summary": wipe,
            })),
        }
    }

    /// Change the LLM endpoint URL for a connected agent. The new LLMCore is built
    /// immediately using the same provider/model/credentials and replaces the old one
    /// in `active_llms`, so the change takes effect on the next task without reconnecting.
    pub(crate) async fn cmd_set_agent_base_url(&self, name: String, url: String) -> KernelResponse {
        // Reject empty/whitespace URLs. An empty string would cause reqwest to build
        // requests against a relative URL, surfacing as "builder error" much later.
        if url.trim().is_empty() {
            return KernelResponse::Error {
                message: "Base URL cannot be empty. Provide a full URL like 'http://host:port'"
                    .to_string(),
            };
        }
        // Look up the agent
        let (agent_id, provider, model) = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(&name) {
                Some(p) if p.status != AgentStatus::Offline => {
                    (p.id, p.provider.clone(), p.model.clone())
                }
                Some(_) => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' is offline — reconnect it first", name),
                    }
                }
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' not found", name),
                    }
                }
            }
        };

        // Build a new LLMCore with the new URL using the same credentials
        let image_resolver = self
            .image_resolver
            .read()
            .expect("image_resolver lock poisoned")
            .clone();
        let new_core: Result<Arc<dyn LLMCore>, String> = match &provider {
            LLMProvider::Ollama => Ok(Arc::new(
                OllamaCore::new(&url, &model)
                    .with_request_timeout(self.config.ollama.request_timeout_secs)
                    .with_context_window(self.config.llm.ollama_context_window)
                    .with_image_resolver(image_resolver.clone()),
            )),
            LLMProvider::OpenAI => {
                let key_result = match self.vault.get(&format!("{}_openai_api_key", name)).await {
                    ok @ Ok(_) => ok,
                    Err(_) => self.vault.get("openai_api_key").await,
                };
                match key_result {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        Ok(Arc::new(
                            OpenAICore::with_base_url(sec, model.clone(), url.clone())
                                .with_image_resolver(image_resolver.clone()),
                        ))
                    }
                    _ => Err("Missing 'openai_api_key' in vault.".to_string()),
                }
            }
            LLMProvider::Anthropic => {
                let key_result = match self.vault.get(&format!("{}_anthropic_api_key", name)).await
                {
                    ok @ Ok(_) => ok,
                    Err(_) => self.vault.get("anthropic_api_key").await,
                };
                match key_result {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        Ok(Arc::new(
                            AnthropicCore::with_base_url(sec, model.clone(), url.clone())
                                .with_max_tokens(self.config.llm.max_tokens)
                                .with_image_resolver(image_resolver.clone()),
                        ))
                    }
                    _ => Err("Missing 'anthropic_api_key' in vault.".to_string()),
                }
            }
            LLMProvider::Gemini => Err(
                "Gemini does not support custom base URLs — reconnect with a different provider."
                    .to_string(),
            ),
            LLMProvider::Custom(ref custom_name) => {
                let catalog_entry_opt = self
                    .provider_catalog
                    .read()
                    .unwrap()
                    .lookup(custom_name)
                    .cloned();
                let sec = if let Some(ref ce) = catalog_entry_opt {
                    if !ce.api_key_env.is_empty() {
                        match self
                            .vault
                            .get(&format!("{}_{}_api_key", name, custom_name))
                            .await
                        {
                            Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                            Err(_) => std::env::var(&ce.api_key_env)
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .map(SecretString::new),
                        }
                    } else {
                        None
                    }
                } else {
                    match self.vault.get(&format!("{}_custom_api_key", name)).await {
                        Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                        Err(_) => match self.vault.get("custom_api_key").await {
                            Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                            _ => None,
                        },
                    }
                };
                Ok(Arc::new(
                    CustomCore::new(sec, model.clone(), url.clone())
                        .with_vision_models(
                            catalog_entry_opt
                                .as_ref()
                                .map(|c| c.vision_models.clone())
                                .unwrap_or_default(),
                        )
                        .with_image_resolver(image_resolver.clone()),
                ))
            }
        };

        let new_core = match new_core {
            Ok(c) => c,
            Err(e) => return KernelResponse::Error { message: e },
        };

        // Swap into active_llms
        self.active_llms.write().await.insert(agent_id, new_core);

        // Persist the new URL on the profile
        self.agent_registry
            .write()
            .await
            .update_base_url(&agent_id, url.clone());

        tracing::info!(agent = %name, url = %url, "Agent base URL updated");
        KernelResponse::Success { data: None }
    }

    pub(crate) async fn cmd_send_agent_message(
        &self,
        from_name: String,
        to_name: String,
        content: String,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let from_agent = match registry.get_by_name(&from_name) {
            Some(a) if a.status != AgentStatus::Offline => a.clone(),
            Some(_) => {
                return KernelResponse::Error {
                    message: format!("Sender agent '{}' is offline", from_name),
                }
            }
            None => {
                return KernelResponse::Error {
                    message: format!("Sender agent '{}' not found", from_name),
                }
            }
        };
        let to_agent = match registry.get_by_name(&to_name) {
            Some(a) if a.status != AgentStatus::Offline => a.clone(),
            Some(_) => {
                return KernelResponse::Error {
                    message: format!("Target agent '{}' is offline", to_name),
                }
            }
            None => {
                return KernelResponse::Error {
                    message: format!("Target agent '{}' not found", to_name),
                }
            }
        };
        drop(registry);

        let now = chrono::Utc::now();
        let mut msg = AgentMessage {
            id: MessageID::new(),
            from: from_agent.id,
            to: agentos_types::MessageTarget::Direct(to_agent.id),
            content: agentos_types::MessageContent::Text(content.clone()),
            reply_to: None,
            timestamp: now,
            trace_id: TraceID::new(),
            signature: None,
            ttl_seconds: 60,
            expires_at: Some(now + chrono::Duration::seconds(60)),
        };

        // Sign the message with the sender's Ed25519 identity
        match self
            .identity_manager
            .sign_message(&from_agent.id, &msg.signing_payload())
            .await
        {
            Ok(sig) => msg.signature = Some(sig),
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to sign message from '{}': {}", from_name, e),
                };
            }
        }

        self.agent_inbox_writer
            .write_message(from_agent.id, from_name.clone(), to_agent.id, content)
            .await;

        match self.message_bus.send_direct(msg).await {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_list_agent_messages(
        &self,
        agent_name: String,
        limit: u32,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        let history = self
            .message_bus
            .get_history(&agent.id, limit as usize)
            .await;
        KernelResponse::AgentMessageList(history)
    }

    pub(crate) async fn cmd_create_agent_group(
        &self,
        group_name: String,
        members: Vec<String>,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let mut member_ids = Vec::new();
        for m in members {
            if let Some(a) = registry.get_by_name(&m) {
                member_ids.push(a.id);
            } else {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", m),
                };
            }
        }
        drop(registry);

        let group_id = GroupID::new();
        self.message_bus.create_group(group_id, member_ids).await;

        KernelResponse::Success {
            data: Some(
                serde_json::json!({ "group_id": group_id.to_string(), "group_name": group_name }),
            ),
        }
    }

    pub(crate) async fn cmd_broadcast_to_group(
        &self,
        from_name: String,
        _group_name: String,
        content: String,
    ) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let from_agent = match registry.get_by_name(&from_name) {
            Some(a) if a.status != AgentStatus::Offline => a.clone(),
            Some(_) => {
                return KernelResponse::Error {
                    message: format!("Sender agent '{}' is offline", from_name),
                };
            }
            None => {
                return KernelResponse::Error {
                    message: format!("Sender agent '{}' not found", from_name),
                };
            }
        };
        drop(registry);

        let now = chrono::Utc::now();
        let mut msg = AgentMessage {
            id: MessageID::new(),
            from: from_agent.id,
            to: agentos_types::MessageTarget::Broadcast,
            content: agentos_types::MessageContent::Text(content),
            reply_to: None,
            timestamp: now,
            trace_id: TraceID::new(),
            signature: None,
            ttl_seconds: 60,
            expires_at: Some(now + chrono::Duration::seconds(60)),
        };

        // Sign the message with the sender's Ed25519 identity
        match self
            .identity_manager
            .sign_message(&from_agent.id, &msg.signing_payload())
            .await
        {
            Ok(sig) => msg.signature = Some(sig),
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to sign broadcast from '{}': {}", from_name, e),
                };
            }
        }

        match self.message_bus.broadcast(msg).await {
            Ok(count) => KernelResponse::Success {
                data: Some(serde_json::json!({ "sent_to": count })),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    /// Called once during `boot()` after all subsystems are ready. For each agent
    /// that was not explicitly disconnected by the user (`manually_offline = false`),
    /// this rebuilds the LLM adapter from stored credentials and brings the agent
    /// back Online without running the onboarding flow again.
    ///
    /// Agents whose vault key is missing are left Offline with an audit event.
    ///
    /// Unlike interactive `connect` which aborts on Unhealthy backends, boot-time
    /// reactivation tolerates them — the operator may start the kernel before LLM
    /// backends are reachable, and aborting here would strand all persisted agents.
    /// The first task will surface any backend errors naturally.
    ///
    /// Ed25519 pubkeys are already pre-registered with the message bus earlier in
    /// `boot()` (`register_pubkey_internal` loop), so signing is available immediately.
    ///
    /// Returns `(reactivated, skipped)` counts for logging.
    pub(crate) async fn auto_reactivate_agents(&self) -> (usize, usize) {
        let candidates: Vec<AgentProfile> = {
            let registry = self.agent_registry.read().await;
            registry
                .list_all()
                .into_iter()
                // `load_from_disk` forces all agents to Offline; the status check
                // pins that invariant so a future change can't double-reactivate.
                .filter(|a| !a.manually_offline && a.status == AgentStatus::Offline)
                .cloned()
                .collect()
        };

        if candidates.is_empty() {
            return (0, 0);
        }

        tracing::info!(
            count = candidates.len(),
            "Auto-reactivating persisted agents from previous kernel session"
        );

        let mut reactivated = 0usize;
        let mut skipped = 0usize;

        for agent in candidates {
            let agent_id = agent.id;
            let agent_name = agent.name.clone();
            let agent_model = agent.model.clone();

            let (llm_adapter, _) = match self
                .build_llm_adapter(
                    &agent_name,
                    &agent.provider,
                    &agent_model,
                    agent.base_url.clone(),
                )
                .await
            {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(
                        agent_name = %agent_name,
                        error = %e,
                        "Auto-reactivation skipped: failed to build LLM adapter"
                    );
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::LLMConnectionFailed,
                        agent_id: Some(agent_id),
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({
                            "name": agent_name,
                            "reason": e.to_string(),
                            "auto_reactivated": false,
                        }),
                        severity: agentos_audit::AuditSeverity::Warn,
                        reversible: false,
                        rollback_ref: None,
                    });
                    skipped += 1;
                    continue;
                }
            };

            // Recover missing Ed25519 identity — edge case where key gen failed at first
            // connect. The boot pre-population loop skips agents without a pubkey, so
            // the bus won't have this agent's key unless we generate and register it now.
            if agent.public_key_hex.is_none() {
                match self.identity_manager.generate_identity(&agent_id).await {
                    Ok(pk) => {
                        if let Err(e) = self
                            .message_bus
                            .register_pubkey_internal(agent_id, pk.clone())
                            .await
                        {
                            tracing::warn!(
                                agent_name = %agent_name,
                                error = %e,
                                "Auto-reactivation: failed to register recovered pubkey"
                            );
                        }
                        self.agent_registry
                            .write()
                            .await
                            .update_public_key(&agent_id, pk);
                        tracing::info!(
                            agent_name = %agent_name,
                            "Auto-reactivation: recovered missing Ed25519 identity"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent_name = %agent_name,
                            error = %e,
                            "Auto-reactivation skipped: failed to generate Ed25519 identity"
                        );
                        skipped += 1;
                        continue;
                    }
                }
            }

            match llm_adapter.health_check().await {
                agentos_llm::HealthStatus::Healthy => {}
                agentos_llm::HealthStatus::Degraded { reason }
                | agentos_llm::HealthStatus::Unhealthy { reason } => {
                    tracing::warn!(
                        agent_name = %agent_name,
                        %reason,
                        "Auto-reactivation: LLM backend not fully healthy at boot — proceeding; first task will surface errors"
                    );
                }
            }

            // Persist Online status before inserting the adapter so no window exists
            // where the agent appears Online but has no adapter in `active_llms`.
            // If the agent was removed between snapshot and now, skip all further setup.
            let reactivated_ok = self.agent_registry.write().await.reactivate(&agent_id);
            if !reactivated_ok {
                tracing::warn!(
                    agent_name = %agent_name,
                    "Auto-reactivation: agent removed from registry between snapshot and reactivation — skipping"
                );
                skipped += 1;
                continue;
            }

            self.active_llms.write().await.insert(agent_id, llm_adapter);

            let agent_home = self.data_dir.join("agents").join(&agent_name);
            if let Err(e) = tokio::fs::create_dir_all(&agent_home).await {
                tracing::warn!(
                    agent_name = %agent_name,
                    error = %e,
                    "Failed to create agent home directory during auto-reactivation"
                );
            }

            self.cost_tracker
                .register_agent(agent_id, agent_name.clone(), AgentBudget::default())
                .await;

            let mut default_specs: Vec<(EventTypeFilter, SubscriptionPriority)> = Vec::new();
            for role in &agent.roles {
                for spec in crate::event_bus::default_subscriptions_for_role(role) {
                    if !default_specs.contains(&spec) {
                        default_specs.push(spec);
                    }
                }
            }
            for (event_type_filter, priority) in default_specs {
                self.event_bus
                    .subscribe(EventSubscription {
                        id: SubscriptionID::new(),
                        agent_id,
                        event_type_filter,
                        filter: None,
                        priority,
                        throttle: ThrottlePolicy::None,
                        enabled: true,
                        created_at: chrono::Utc::now(),
                    })
                    .await;
            }

            self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::AgentReconnected,
                agent_id: Some(agent_id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "name": agent_name,
                    "model": agent_model,
                    "auto_reactivated": true,
                }),
                severity: agentos_audit::AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            });

            // Do NOT emit AgentAdded here — reactivated agents are being restored to a
            // prior session state, not added anew. Emitting AgentAdded would trigger
            // event-subscribed agents to queue response tasks for every peer restored
            // on restart, causing N×(N-1) spurious tasks. The self-exclusion filter in
            // event_dispatch only prevents the added agent from seeing its own event;
            // every other reactivated peer would still receive one per agent restored.
            // The audit entry above is the sole notification; genuine new-agent events
            // come from cmd_connect_agent (only when !is_reconnect).
            tracing::info!(
                agent_name = %agent_name,
                agent_id = %agent_id,
                "Agent auto-reactivated on kernel restart"
            );

            reactivated += 1;
        }

        (reactivated, skipped)
    }
}

fn default_permissions_for_agent(name: &str) -> PermissionSet {
    let mut perms = PermissionSet::new();

    // Filesystem — shared user data read+write, own namespace full access
    perms.grant("fs.user_data".to_string(), true, true, false, None);
    perms.grant(format!("fs:agents/{name}/"), true, true, true, None);

    // Application logs — read-only (log-reader)
    perms.grant("fs.app_logs".to_string(), true, false, false, None);

    // Memory — coarse read gate + per-scope read+write
    perms.grant("memory.read".to_string(), true, false, false, None);
    perms.grant("memory.semantic".to_string(), true, true, false, None);
    perms.grant("memory.episodic".to_string(), true, true, false, None);
    perms.grant("memory.procedural".to_string(), true, true, false, None);

    // Memory blocks — read+write for named memory blocks
    perms.grant("memory.blocks".to_string(), true, true, false, None);

    // Context memory — read+write (context-memory-read, context-memory-update)
    perms.grant("memory.context".to_string(), true, true, false, None);

    // Agent registry — read-only (agent-self, agent-list, agent-manual)
    perms.grant("agent.registry".to_string(), true, false, false, None);

    // Agent messaging — execute (agent-message, task-delegate)
    perms.grant_op("agent.message".to_string(), PermissionOp::Execute, None);

    // Agent calls — execute (agent-call for direct inter-agent invocations)
    perms.grant_op("agent.call".to_string(), PermissionOp::Execute, None);

    // Agent spawning — execute (spawn-agent, await-agents, verify-output)
    perms.grant_op("agent.spawn".to_string(), PermissionOp::Execute, None);

    // User interaction — ask-user (execute) and notify-user (write)
    perms.grant_op("user.interact".to_string(), PermissionOp::Execute, None);
    perms.grant("user.notify".to_string(), false, true, false, None);

    // Hardware system info — read-only (hardware-info, sys-monitor)
    perms.grant("hardware.system".to_string(), true, false, false, None);

    // Network outbound — execute (http-client, web-fetch with SSRF protection)
    perms.grant_op("network.outbound".to_string(), PermissionOp::Execute, None);

    // Process listing — read-only (sys-monitor)
    perms.grant("process.list".to_string(), true, false, false, None);
    // Note: process.exec (shell-exec) is NOT granted by default — it is
    // added dynamically for autonomous/background tasks, or can be granted
    // explicitly via `agentos perm grant <agent> process.exec:x`.

    // Task query — read-only (task-list, task-status)
    perms.grant("task.query".to_string(), true, false, false, None);

    // Escalation query — query (escalation-status)
    perms.grant_op("escalation.query".to_string(), PermissionOp::Query, None);

    // Event stream — observe (coarse gate for the four event-* tools).
    // Per-category permissions below decide which event categories an agent
    // is actually allowed to subscribe to via `event-subscribe`.
    perms.grant_op("events.stream".to_string(), PermissionOp::Observe, None);

    // Event categories an agent can observe by default. Matches the universal
    // role-seeded subscriptions (AgentAdded, DirectMessageReceived,
    // DelegationReceived) plus task lifecycle which agents commonly need.
    perms.grant_op(
        "events.agent_lifecycle".to_string(),
        PermissionOp::Observe,
        None,
    );
    perms.grant_op(
        "events.agent_communication".to_string(),
        PermissionOp::Observe,
        None,
    );
    perms.grant_op(
        "events.task_lifecycle".to_string(),
        PermissionOp::Observe,
        None,
    );

    // Scratchpad — read+write (scratch-read, scratch-write, scratch-list)
    perms.grant("scratchpad".to_string(), true, true, false, None);

    perms
}

#[cfg(test)]
mod tests {
    use super::parse_provider_name;
    use agentos_types::LLMProvider;

    #[test]
    fn parse_provider_name_known_variants() {
        assert_eq!(parse_provider_name("ollama"), LLMProvider::Ollama);
        assert_eq!(parse_provider_name("OpenAI"), LLMProvider::OpenAI);
        assert_eq!(parse_provider_name("anthropic"), LLMProvider::Anthropic);
        assert_eq!(parse_provider_name("gemini"), LLMProvider::Gemini);
    }

    #[test]
    fn parse_provider_name_custom_and_catalog() {
        // Bare catalog name → Custom(name); resolved against the catalog at build.
        assert_eq!(
            parse_provider_name("nvidia"),
            LLMProvider::Custom("nvidia".to_string())
        );
        // `custom:<name>` form.
        assert_eq!(
            parse_provider_name("custom:groq"),
            LLMProvider::Custom("groq".to_string())
        );
        // Bare `custom`.
        assert_eq!(
            parse_provider_name("custom"),
            LLMProvider::Custom("custom".to_string())
        );
        // Empty name after the colon normalizes to "custom" rather than "".
        assert_eq!(
            parse_provider_name("custom:"),
            LLMProvider::Custom("custom".to_string())
        );
    }
}
