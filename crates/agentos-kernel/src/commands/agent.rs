use crate::event_bus::default_subscriptions_for_role;
use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_llm::{AnthropicCore, CustomCore, GeminiCore, LLMCore, OllamaCore, OpenAICore};
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

impl Kernel {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn cmd_connect_agent(
        &self,
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
        roles: Vec<String>,
        test_mode: bool,
        extra_permissions: Vec<String>,
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

        // Instantiate LLMCore based on provider
        let core: Result<Arc<dyn LLMCore>, String> = match &provider {
            LLMProvider::Ollama => {
                let host = base_url
                    .or_else(|| {
                        std::env::var("AGENTOS_OLLAMA_HOST")
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                    })
                    .unwrap_or_else(|| self.config.ollama.host.clone());
                Ok(Arc::new(
                    OllamaCore::new(&host, &model)
                        .with_request_timeout(self.config.ollama.request_timeout_secs)
                        .with_context_window(self.config.llm.ollama_context_window),
                ))
            }
            LLMProvider::OpenAI => {
                let key_result = match self.vault.get(&format!("{}_openai_api_key", name)).await {
                    ok @ Ok(_) => ok,
                    Err(_) => self.vault.get("openai_api_key").await,
                };
                match key_result {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        let resolved_base_url = base_url
                            .or_else(|| {
                                std::env::var("AGENTOS_OPENAI_BASE_URL")
                                    .ok()
                                    .filter(|s| !s.trim().is_empty())
                            })
                            .or_else(|| self.config.llm.openai_base_url.clone());
                        if let Some(url) = resolved_base_url {
                            Ok(Arc::new(OpenAICore::with_base_url(sec, model.clone(), url)))
                        } else {
                            Ok(Arc::new(OpenAICore::new(sec, model.clone())))
                        }
                    }
                    _ => {
                        Err("Missing 'openai_api_key' in vault. Please store it first.".to_string())
                    }
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
                        let base_url =
                            base_url.or_else(|| self.config.llm.anthropic_base_url.clone());
                        let adapter = if let Some(url) = base_url {
                            AnthropicCore::with_base_url(sec, model.clone(), url)
                        } else {
                            AnthropicCore::new(sec, model.clone())
                        };
                        Ok(Arc::new(
                            adapter.with_max_tokens(self.config.llm.max_tokens),
                        ))
                    }
                    _ => Err(
                        "Missing 'anthropic_api_key' in vault. Please store it first.".to_string(),
                    ),
                }
            }
            LLMProvider::Gemini => {
                let key_result = match self.vault.get(&format!("{}_gemini_api_key", name)).await {
                    ok @ Ok(_) => ok,
                    Err(_) => self.vault.get("gemini_api_key").await,
                };
                match key_result {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        Ok(Arc::new(GeminiCore::new(sec, model.clone())))
                    }
                    _ => {
                        Err("Missing 'gemini_api_key' in vault. Please store it first.".to_string())
                    }
                }
            }
            LLMProvider::Custom(_) => {
                let sec = match self.vault.get(&format!("{}_custom_api_key", name)).await {
                    Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                    Err(_) => match self.vault.get("custom_api_key").await {
                        Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                        _ => None,
                    },
                };
                let url = match base_url
                    .or_else(|| {
                        std::env::var("AGENTOS_LLM_URL")
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                    })
                    .or_else(|| self.config.llm.custom_base_url.clone())
                {
                    Some(url) => url,
                    None => {
                        return KernelResponse::Error {
                            message: "Missing custom LLM endpoint. Provide --base-url, set AGENTOS_LLM_URL, or configure llm.custom_base_url in config.".to_string(),
                        };
                    }
                };
                Ok(Arc::new(CustomCore::new(sec, model.clone(), url)))
            }
        };

        let llm_adapter = match core {
            Ok(adapter) => adapter,
            Err(e) => {
                return KernelResponse::Error { message: e };
            }
        };

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
                created_at,
                is_reconnect,
            ) = match registry.get_by_name(&name) {
                Some(existing) if existing.provider == provider && existing.model == model => (
                    existing.id,
                    existing.public_key_hex.clone(),
                    existing.permissions.clone(),
                    existing.roles.clone(),
                    existing.description.clone(),
                    existing.created_at,
                    true,
                ),
                _ => (
                    AgentID::new(),
                    None,
                    default_permissions_for_agent(&name),
                    vec![],
                    String::new(),
                    now,
                    false,
                ),
            };

            // Apply extra permissions supplied via --grant flags
            let mut persisted_permissions = persisted_permissions;
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
                description: persisted_description,
                created_at,
                last_active: now,
                public_key_hex,
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

        // Emit AgentAdded event
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

        // Queue an onboarding or test-evaluation task for the agent.
        // New agents always get an onboarding prompt so they orient themselves in the
        // ecosystem. Reconnecting agents only get a task when --test is explicitly passed.
        let mut onboarding_task_id_opt: Option<TaskID> = None;
        if !is_reconnect || test_mode {
            let prompt = if test_mode {
                format!(
                    r#"[TEST MODE — ECOSYSTEM EVALUATION]

You are {agent_name}, a {agent_model} agent that has just been connected to AgentOS in test mode.
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
                    agent_model = agent_model,
                )
            } else {
                format!(
                    r#"[ONBOARDING — WELCOME TO AGENTOS]

You are {agent_name}, powered by {agent_model}, and you have just been connected to AgentOS — an LLM-native operating system where AI agents are first-class citizens.

Take a moment to orient yourself:
1. Discover your available tools by listing them — this is how you interact with the system.
2. Check your permissions and understand what you can and cannot do.
3. Try reading and writing to your memory — this is how you persist knowledge across tasks.
4. Look around the filesystem if you have access.
5. If other agents are online, introduce yourself.

Once you have explored, briefly summarise what you found and confirm you are ready to receive tasks."#,
                    agent_name = agent_name,
                    agent_model = agent_model,
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
        registry.update_status(&agent_id, AgentStatus::Offline);
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
                    message: format!("Failed to sign message from '{}': {}", from_name, e),
                };
            }
        }

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

    // Agent registry — read-only (agent-self, agent-list, agent-manual)
    perms.grant("agent.registry".to_string(), true, false, false, None);

    // Agent messaging — execute (agent-message, task-delegate)
    perms.grant_op("agent.message".to_string(), PermissionOp::Execute, None);

    // Hardware system info — read-only (hardware-info, sys-monitor)
    perms.grant("hardware.system".to_string(), true, false, false, None);

    // Network outbound — execute (http-client, web-fetch with SSRF protection)
    perms.grant_op("network.outbound".to_string(), PermissionOp::Execute, None);

    // Process listing — read-only (sys-monitor)
    perms.grant("process.list".to_string(), true, false, false, None);
    // Note: process.exec (shell-exec) is NOT granted by default — it is
    // added dynamically for autonomous/background tasks, or can be granted
    // explicitly via `agentctl perm grant <agent> process.exec:x`.

    // Task query — read-only (task-list, task-status)
    perms.grant("task.query".to_string(), true, false, false, None);

    // Escalation query — query (escalation-status)
    perms.grant_op("escalation.query".to_string(), PermissionOp::Query, None);

    // Event stream — observe (subscribe/unsubscribe to kernel events)
    perms.grant_op("events.stream".to_string(), PermissionOp::Observe, None);

    perms
}
