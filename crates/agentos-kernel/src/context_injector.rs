use crate::injection_scanner::ThreatLevel;
use crate::kernel::Kernel;
use crate::system_prompt::{self, ChannelHint, SubAgentContext, SystemPromptContext};
use agentos_types::*;

impl Kernel {
    /// Assemble initial task context: build system prompt, create context window,
    /// push user prompt, record to episodic memory, run injection scan, and build
    /// the adaptive retrieval plan.
    ///
    /// Returns `(system_prompt, tools_desc, agent_directory, retrieval_plan)` on success.
    /// Returns `Err` if execution should be aborted (e.g., high-confidence injection detected).
    pub(crate) async fn setup_task_context(
        &self,
        task: &AgentTask,
        task_trace_id: &TraceID,
    ) -> anyhow::Result<(String, String, String, crate::retrieval_gate::RetrievalPlan)> {
        // 1. Collect elements for CompilationInputs
        let base_tools_desc = self.tool_registry.read().await.tools_for_prompt();
        // Append recently-used tool hint from the in-memory LRU (cap 10 per agent).
        let tools_desc = {
            let lru_guard = self.agent_tool_lru.read().await;
            if let Some(recent) = lru_guard.get(&task.agent_id) {
                if !recent.is_empty() {
                    let names: Vec<&str> = recent.iter().map(|s| s.as_str()).collect();
                    format!("{}\nRecently used: {}.", base_tools_desc, names.join(", "))
                } else {
                    base_tools_desc
                }
            } else {
                base_tools_desc
            }
        };
        let agent_directory = self.build_agent_directory(&task.agent_id).await;

        // Build system prompt from the canonical builder — same prompt structure
        // for every context window (task execution, web UI chat, sub-agents).
        let (agent_name, agent_description, agent_roles, custom_instructions) = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_id(&task.agent_id) {
                Some(profile) => (
                    profile.name.clone(),
                    profile.description.clone(),
                    profile.roles.clone(),
                    profile.system_prompt.clone(),
                ),
                None => (
                    format!("agent-{}", &task.agent_id.to_string()[..8]),
                    String::new(),
                    vec![],
                    None,
                ),
            }
        };

        let sub_agent = task.parent_task_id.map(|parent_id| SubAgentContext {
            parent_task_id: parent_id.to_string(),
            spawn_depth: task.spawn_depth,
        });

        // Tier-0 channel awareness: pull connected channels at task start so the
        // agent always knows what's available. Skipped silently on registry
        // errors — better to omit the block than fail the whole task.
        let connected_channels: Vec<ChannelHint> = match self.channel_registry.list_active().await {
            Ok(list) => list
                .into_iter()
                .filter(|c| c.active)
                .map(|c| ChannelHint {
                    name: c.display_name,
                    kind: c.kind.to_string(),
                })
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to list channels for system prompt; omitting block");
                Vec::new()
            }
        };

        let system_prompt = system_prompt::build_system_prompt(&SystemPromptContext {
            agent_name,
            agent_description,
            agent_roles,
            custom_instructions,
            sub_agent,
            // Task execution does not stream through the chat output filter,
            // so the `<final>` enforcement convention does not apply.
            enforce_final_tag: false,
            timezone: system_prompt::local_timezone_str(),
            connected_channels,
        });

        // We initialize context with empty string; Compiler injects the true system prompt
        // into the compiled ContextWindow at each iteration.
        // `create_context` is idempotent: if a context was pre-seeded via `seed_from_slice`
        // (e.g. parent context handoff) the existing window is preserved.
        self.context_manager
            .create_context(task.id, task.agent_id, "")
            .await;

        // 2. Push the user's prompt into context (pinned — original task is always kept).
        // Guard against duplicates on task resume (escalation approval, checkpoint restore).
        if !self.context_manager.is_prompt_pushed(&task.id).await {
            self.context_manager
                .push_entry(
                    &task.id,
                    ContextEntry {
                        role: ContextRole::User,
                        parts: vec![ContentPart::Text {
                            text: task.original_prompt.clone(),
                        }],
                        timestamp: chrono::Utc::now(),
                        metadata: None,
                        importance: 0.95,
                        pinned: true,
                        reference_count: 0,
                        partition: ContextPartition::default(),
                        category: ContextCategory::Task,
                        is_summary: false,
                    },
                )
                .await
                .ok();
            self.context_manager.mark_prompt_pushed(&task.id).await;
        }

        if let Err(e) = self
            .episodic_memory
            .record(agentos_memory::EpisodeRecordInput {
                task_id: &task.id,
                agent_id: &task.agent_id,
                entry_type: agentos_memory::EpisodeType::UserPrompt,
                content: &task.original_prompt,
                summary: Some("User prompt received"),
                metadata: None,
                trace_id: task_trace_id,
            })
            .await
        {
            tracing::warn!(task_id = %task.id, error = %e, "Failed to record episodic memory");
        }

        // 2.1 Injection scan on user prompt (Spec §6 — scan ALL untrusted inputs)
        {
            let prompt_scan = self.injection_scanner.scan(&task.original_prompt);
            if prompt_scan.is_suspicious {
                let pattern_names: Vec<&str> =
                    prompt_scan.matches.iter().map(|m| m.pattern_name).collect();
                let threat = format!("{:?}", prompt_scan.max_threat);
                let trace_id = *task_trace_id;

                tracing::warn!(
                    "Task {} user prompt contains injection patterns: {:?} (threat: {})",
                    task.id,
                    pattern_names,
                    threat
                );
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::RiskEscalation,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "injection_scan": true,
                        "source": "user_prompt",
                        "patterns": pattern_names,
                        "max_threat": threat,
                    }),
                    severity: agentos_audit::AuditSeverity::Security,
                    reversible: false,
                    rollback_ref: None,
                });

                let threat_level = prompt_scan
                    .max_threat
                    .as_ref()
                    .map(|t| format!("{:?}", t))
                    .unwrap_or_else(|| "unknown".to_string());
                let severity = match prompt_scan.max_threat {
                    Some(ThreatLevel::High) => EventSeverity::Critical,
                    Some(ThreatLevel::Medium) => EventSeverity::Warning,
                    Some(ThreatLevel::Low) | None => EventSeverity::Info,
                };
                let chain_depth = task
                    .trigger_source
                    .as_ref()
                    .map(|ts| ts.chain_depth + 1)
                    .unwrap_or(0);
                self.emit_event_with_trace(
                    EventType::PromptInjectionAttempt,
                    EventSource::SecurityEngine,
                    severity,
                    serde_json::json!({
                        "task_id": task.id.to_string(),
                        "agent_id": task.agent_id.to_string(),
                        "source": "user_prompt",
                        "tool_name": serde_json::Value::Null,
                        "threat_level": threat_level,
                        "pattern_count": prompt_scan.matches.len(),
                        "patterns": prompt_scan.matches.iter().map(|m| m.pattern_name).collect::<Vec<_>>(),
                        "agent_intent_payload": serde_json::Value::Null,
                        "suspicious_content": Self::truncate_for_prompt_payload(&task.original_prompt, 600),
                    }),
                    chain_depth,
                    Some(trace_id),
                                Some(task.agent_id),
                Some(task.id),
                )
                .await;

                // High-confidence injection: block execution and require human review
                if prompt_scan.max_threat == Some(crate::injection_scanner::ThreatLevel::High) {
                    self.escalation_manager
                        .create_escalation(
                            task.id,
                            task.agent_id,
                            crate::kernel_action::EscalationReason::SafetyConcern,
                            format!(
                                "User prompt contains high-confidence injection patterns: {:?}",
                                pattern_names
                            ),
                            "Review the user prompt before allowing task execution.".to_string(),
                            vec![
                                "Allow — continue execution".to_string(),
                                "Deny — cancel task".to_string(),
                            ],
                            "high".to_string(),
                            true,
                            trace_id,
                            None, // auto_action: default deny on expiry
                        )
                        .await;
                    if let Err(e) = self
                        .scheduler
                        .update_state(&task.id, TaskState::Waiting)
                        .await
                    {
                        tracing::error!(error = %e, task_id = %task.id, "Failed to update task state to Waiting — task may be stuck in Running state");
                    }
                    // Injection is a security event — clear tainted context and
                    // intent history so the adversarial payload cannot be re-ingested
                    // if the task is ever resumed. Context preservation is only
                    // appropriate for legitimate escalation pauses (tool hard-approval).
                    self.context_manager.remove_context(&task.id).await;
                    self.intent_validator.remove_task(&task.id).await;
                    anyhow::bail!("Task paused: high-confidence injection detected in user prompt");
                }
            }
        }

        // 2.5. Adaptive retrieval gate: classify once, then refresh retrieval results per
        // iteration so mid-task memory writes are visible in subsequent compile passes.
        let retrieval_plan = self.retrieval_gate.classify(&task.original_prompt);

        Ok((system_prompt, tools_desc, agent_directory, retrieval_plan))
    }
}
