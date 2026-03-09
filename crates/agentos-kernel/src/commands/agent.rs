use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_llm::{AnthropicCore, CustomCore, GeminiCore, OllamaCore, OpenAICore, LLMCore};
use agentos_types::*;
use secrecy::SecretString;
use std::sync::Arc;

impl Kernel {
    pub(crate) async fn cmd_connect_agent(
        &self,
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
    ) -> KernelResponse {
        let now = chrono::Utc::now();
        let agent_id = AgentID::new();

        // Instantiate LLMCore based on provider
        let core: Result<Arc<dyn LLMCore>, String> = match &provider {
            LLMProvider::Ollama => {
                let host = base_url.unwrap_or_else(|| self.config.ollama.host.clone());
                Ok(Arc::new(OllamaCore::new(&host, &model)))
            }
            LLMProvider::OpenAI => {
                match self
                    .vault
                    .get(&format!("{}_openai_api_key", name))
                    .or_else(|_| self.vault.get("openai_api_key"))
                {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        if let Some(url) = base_url {
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
                match self
                    .vault
                    .get(&format!("{}_anthropic_api_key", name))
                    .or_else(|_| self.vault.get("anthropic_api_key"))
                {
                    Ok(entry) => {
                        let sec = SecretString::new(entry.as_str().to_string());
                        Ok(Arc::new(AnthropicCore::new(sec, model.clone())))
                    }
                    _ => Err(
                        "Missing 'anthropic_api_key' in vault. Please store it first.".to_string(),
                    ),
                }
            }
            LLMProvider::Gemini => {
                match self
                    .vault
                    .get(&format!("{}_gemini_api_key", name))
                    .or_else(|_| self.vault.get("gemini_api_key"))
                {
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
                let sec = match self
                    .vault
                    .get(&format!("{}_custom_api_key", name))
                    .or_else(|_| self.vault.get("custom_api_key"))
                {
                    Ok(entry) => Some(SecretString::new(entry.as_str().to_string())),
                    _ => None,
                };
                let url = base_url.unwrap_or_else(|| "http://localhost:8000/v1".to_string());
                Ok(Arc::new(CustomCore::new(sec, model.clone(), url)))
            }
        };

        let llm_adapter = match core {
            Ok(adapter) => adapter,
            Err(e) => {
                return KernelResponse::Error { message: e };
            }
        };

        let profile = AgentProfile {
            id: agent_id,
            name,
            provider,
            model,
            status: AgentStatus::Online,
            permissions: PermissionSet::new(),
            roles: vec!["base".to_string()],
            current_task: None,
            description: String::new(),
            created_at: now,
            last_active: now,
        };

        let agent_name = profile.name.clone();
        let agent_model = profile.model.clone();

        {
            let mut registry = self.agent_registry.write().await;
            registry.register(profile.clone());
        }

        {
            let mut active = self.active_llms.write().await;
            active.insert(agent_id, llm_adapter);
        }

        self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::AgentConnected,
                agent_id: Some(agent_id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "name": agent_name, "model": agent_model }),
                severity: agentos_audit::AuditSeverity::Info,
            });

        KernelResponse::Success {
            data: Some(serde_json::json!({ "agent_id": agent_id.to_string() })),
        }
    }

    pub(crate) async fn cmd_list_agents(&self) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let agents: Vec<AgentProfile> = registry.list_all().into_iter().cloned().collect();
        KernelResponse::AgentList(agents)
    }

    pub(crate) async fn cmd_disconnect_agent(&self, agent_id: AgentID) -> KernelResponse {
        let mut registry = self.agent_registry.write().await;
        if registry.get_by_id(&agent_id).is_none() {
            return KernelResponse::Error {
                message: format!("Agent '{}' not found", agent_id),
            };
        }
        registry.remove(&agent_id);
        drop(registry);

        self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::AgentDisconnected,
                agent_id: Some(agent_id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({}),
                severity: agentos_audit::AuditSeverity::Info,
            });

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
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Sender agent '{}' not found", from_name),
                }
            }
        };
        let to_agent = match registry.get_by_name(&to_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Target agent '{}' not found", to_name),
                }
            }
        };
        drop(registry);

        let msg = AgentMessage {
            id: MessageID::new(),
            from: from_agent.id,
            to: agentos_types::MessageTarget::Direct(to_agent.id),
            content: agentos_types::MessageContent::Text(content),
            reply_to: None,
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
        };

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
        _group_name: String,
        content: String,
    ) -> KernelResponse {
        let msg = AgentMessage {
            id: MessageID::new(),
            from: AgentID::new(),
            to: agentos_types::MessageTarget::Broadcast,
            content: agentos_types::MessageContent::Text(content),
            reply_to: None,
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
        };

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
