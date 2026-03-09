use crate::kernel::Kernel;
use crate::tool_call::parse_tool_call;
use agentos_sandbox::SandboxConfig;
use agentos_sandbox::SandboxExecutor;
use agentos_tools::traits::ToolExecutionContext;
use agentos_types::*;
use std::sync::Arc;
use std::time::Duration;

impl Kernel {
    pub(crate) async fn task_executor_loop(self: &Arc<Self>) {
        loop {
            if self.scheduler.running_count().await >= self.config.kernel.max_concurrent_tasks {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            if let Some(task) = self.scheduler.dequeue().await {
                let kernel = self.clone();
                tokio::spawn(async move {
                    kernel.execute_task(&task).await;
                });
            } else {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    /// Validate a tool call against the capability token and permission system.
    fn validate_tool_call(
        &self,
        task: &AgentTask,
        tool_call: &crate::tool_call::ParsedToolCall,
        trace_id: TraceID,
    ) -> Result<(), String> {
        let intent = IntentMessage {
            id: MessageID::new(),
            sender_token: task.capability_token.clone(),
            intent_type: tool_call.intent_type,
            target: IntentTarget::Kernel,
            payload: SemanticPayload {
                schema: tool_call.tool_name.clone(),
                data: tool_call.payload.clone(),
            },
            context_ref: ContextID::new(),
            priority: task.priority,
            timeout_ms: task.timeout.as_millis() as u32,
            trace_id,
            timestamp: chrono::Utc::now(),
        };

        // Validate payload against registered JSON Schema (if any)
        self.schema_registry
            .validate(&tool_call.tool_name, &tool_call.payload)
            .map_err(|e| e)?;

        let required_perms = self
            .tool_runner
            .get_required_permissions(&tool_call.tool_name)
            .unwrap_or_default();

        let required_for_validate: Vec<(String, PermissionOp)> = required_perms;

        self.capability_engine
            .validate_intent(&task.capability_token, &intent, &required_for_validate)
            .map_err(|e| format!("{}", e))
    }

    /// Execute a single task synchronously: assemble context, call LLM, process tool calls, repeat.
    pub(crate) async fn execute_task_sync(
        &self,
        task: &AgentTask,
    ) -> Result<String, anyhow::Error> {
        let agent = {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&task.agent_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", task.agent_id))?
        };

        let llm = {
            let active = self.active_llms.read().await;
            active.get(&agent.id).cloned()
        };

        let llm = match llm {
            Some(adapter) => adapter,
            None => {
                return Err(anyhow::anyhow!(
                    "LLM adapter for agent {} not connected",
                    agent.name
                ));
            }
        };

        // 1. Create context with system prompt
        let tools_desc = self.tool_registry.read().await.tools_for_prompt();
        let system_prompt = format!(
            "You are an AI agent operating inside AgentOS.\n\
             Available tools:\n{}\n\
             To use a tool, respond with a JSON block:\n\
             ```json\n{{\"tool\": \"tool-name\", \"intent_type\": \"read|write\", \"payload\": {{...}}}}\n```\n\
             When done, provide your final answer as plain text without any tool call blocks.",
            tools_desc
        );

        let mut agent_directory = String::from("\n\n[AGENT_DIRECTORY]\nYou are operating inside AgentOS. The following agents are available:\n");
        let agents = self
            .agent_registry
            .read()
            .await
            .list_all()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        for opt_agent in agents {
            if opt_agent.id == task.agent_id {
                continue;
            }
            let status = match opt_agent.current_task {
                Some(tid) => format!("Busy ({})", tid),
                None => "Idle".to_string(),
            };

            let perms = self
                .capability_engine
                .get_permissions(&opt_agent.id)
                .unwrap_or_default();
            let mut perm_strs = Vec::new();
            for e in perms.entries {
                let r = if e.read { "r" } else { "" };
                let w = if e.write { "w" } else { "" };
                let x = if e.execute { "x" } else { "" };
                perm_strs.push(format!("{}:{}{}{}", e.resource, r, w, x));
            }
            let perm_str = if perm_strs.is_empty() {
                "None".to_string()
            } else {
                perm_strs.join(", ")
            };

            let provider_str = match opt_agent.provider {
                agentos_types::LLMProvider::Anthropic => "anthropic",
                agentos_types::LLMProvider::OpenAI => "openai",
                agentos_types::LLMProvider::Ollama => "ollama",
                agentos_types::LLMProvider::Gemini => "gemini",
                agentos_types::LLMProvider::Custom(_) => "custom",
            };

            agent_directory.push_str(&format!(
                "\n- {} ({}/{}) — Status: {}\n  Permissions: {}",
                opt_agent.name, provider_str, opt_agent.model, status, perm_str
            ));
        }
        agent_directory.push_str("\n\nTo message an agent: use the agent-message tool\nTo delegate a subtask: use the task-delegate tool\n[/AGENT_DIRECTORY]");

        let system_prompt = format!("{}{}", system_prompt, agent_directory);
        self.context_manager
            .create_context(task.id, &system_prompt)
            .await;

        // 2. Push the user's prompt into context
        self.context_manager
            .push_entry(
                &task.id,
                ContextEntry {
                    role: ContextRole::User,
                    content: task.original_prompt.clone(),
                    timestamp: chrono::Utc::now(),
                    metadata: None,
                },
            )
            .await
            .ok();

        self.episodic_memory
            .record(
                &task.id,
                &task.agent_id,
                agentos_memory::EpisodeType::UserPrompt,
                &task.original_prompt,
                Some("User prompt received"),
                None,
                &TraceID::new(),
            )
            .ok();

        // 3. Agent loop: LLM → parse → tool call → push result → repeat
        let max_iterations = 10;
        let mut final_answer = String::new();

        for iteration in 0..max_iterations {
            let context = match self.context_manager.get_context(&task.id).await {
                Ok(ctx) => ctx,
                Err(_) => break,
            };

            tracing::info!("Task {} iteration {}: calling LLM", task.id, iteration);

            let inference = match llm.infer(&context).await {
                Ok(result) => result,
                Err(e) => {
                    self.context_manager.remove_context(&task.id).await;
                    anyhow::bail!("LLM error: {}", e);
                }
            };

            crate::metrics::record_inference(
                llm.provider_name(),
                llm.model_name(),
                inference.tokens_used.prompt_tokens,
                inference.tokens_used.completion_tokens,
                inference.duration_ms,
            );
            tracing::info!(
                "Task {} LLM responded ({} tokens, {}ms)",
                task.id,
                inference.tokens_used.total_tokens,
                inference.duration_ms
            );

            // Push assistant response into context
            self.context_manager
                .push_entry(
                    &task.id,
                    ContextEntry {
                        role: ContextRole::Assistant,
                        content: inference.text.clone(),
                        timestamp: chrono::Utc::now(),
                        metadata: None,
                    },
                )
                .await
                .ok();

            self.episodic_memory
                .record(
                    &task.id,
                    &task.agent_id,
                    agentos_memory::EpisodeType::LLMResponse,
                    &inference.text,
                    Some(&format!(
                        "LLM response ({} tokens)",
                        inference.tokens_used.total_tokens
                    )),
                    None,
                    &TraceID::new(),
                )
                .ok();

            // Check for tool call
            match parse_tool_call(&inference.text) {
                Some(tool_call) => {
                    tracing::info!(
                        "Task {} tool call: {} ({:?})",
                        task.id,
                        tool_call.tool_name,
                        tool_call.intent_type
                    );

                    let trace_id = TraceID::new();
                    if let Err(denial_reason) = self.validate_tool_call(task, &tool_call, trace_id)
                    {
                        tracing::warn!(
                            "Task {} permission denied for tool {}: {}",
                            task.id,
                            tool_call.tool_name,
                            denial_reason
                        );
                        self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::PermissionDenied,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({
                                    "tool": tool_call.tool_name,
                                    "intent_type": format!("{:?}", tool_call.intent_type),
                                    "reason": denial_reason,
                                }),
                                severity: agentos_audit::AuditSeverity::Security,
                            });

                        let error_result = serde_json::json!({
                            "error": format!(
                                "Permission denied: {}",
                                denial_reason
                            )
                        });
                        self.context_manager
                            .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                            .await
                            .ok();
                        continue;
                    }

                    self.audit_log(agentos_audit::AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id,
                            event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
                            agent_id: Some(task.agent_id),
                            task_id: Some(task.id),
                            tool_id: None,
                            details: serde_json::json!({ "tool": tool_call.tool_name }),
                            severity: agentos_audit::AuditSeverity::Info,
                        });

                    self.episodic_memory
                        .record(
                            &task.id,
                            &task.agent_id,
                            agentos_memory::EpisodeType::ToolCall,
                            &format!(
                                "Tool: {} Payload: {}",
                                tool_call.tool_name, tool_call.payload
                            ),
                            Some(&format!("Called tool '{}'", tool_call.tool_name)),
                            None,
                            &trace_id,
                        )
                        .ok();

                    let exec_context = ToolExecutionContext {
                        data_dir: self.data_dir.clone(),
                        task_id: task.id,
                        agent_id: task.agent_id,
                        trace_id,
                        permissions: task.capability_token.permissions.clone(),
                        vault: Some(self.vault.clone()),
                        hal: Some(self.hal.clone()),
                    };

                    let tool_start = std::time::Instant::now();
                    let tool_result = {
                        let sandbox_config = {
                            let registry = self.tool_registry.read().await;
                            registry
                                .get_by_name(&tool_call.tool_name)
                                .map(|t| SandboxConfig::from_manifest(&t.manifest.sandbox))
                        };

                        if let Some(config) = sandbox_config {
                            let timeout = Duration::from_millis(config.max_cpu_ms.max(5000));
                            match self
                                .sandbox
                                .spawn(
                                    &tool_call.tool_name,
                                    tool_call.payload.clone(),
                                    &config,
                                    timeout,
                                )
                                .await
                            {
                                Ok(sandbox_result) => {
                                    SandboxExecutor::parse_result(&sandbox_result)
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        tool = %tool_call.tool_name,
                                        error = %e,
                                        "Sandbox spawn failed, falling back to in-process execution"
                                    );
                                    self.tool_runner
                                        .execute(
                                            &tool_call.tool_name,
                                            tool_call.payload,
                                            exec_context,
                                        )
                                        .await
                                }
                            }
                        } else {
                            self.tool_runner
                                .execute(&tool_call.tool_name, tool_call.payload, exec_context)
                                .await
                        }
                    };

                    match tool_result {
                        Ok(result) => {
                            crate::metrics::record_tool_execution(
                                &tool_call.tool_name,
                                tool_start.elapsed().as_millis() as u64,
                                true,
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                    timestamp: chrono::Utc::now(),
                                    trace_id,
                                    event_type:
                                        agentos_audit::AuditEventType::ToolExecutionCompleted,
                                    agent_id: Some(task.agent_id),
                                    task_id: Some(task.id),
                                    tool_id: None,
                                    details: serde_json::json!({ "tool": tool_call.tool_name }),
                                    severity: agentos_audit::AuditSeverity::Info,
                                });

                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &result)
                                .await
                                .ok();

                            self.episodic_memory
                                .record(
                                    &task.id,
                                    &task.agent_id,
                                    agentos_memory::EpisodeType::ToolResult,
                                    &result.to_string(),
                                    Some(&format!("Tool '{}' succeeded", tool_call.tool_name)),
                                    None,
                                    &trace_id,
                                )
                                .ok();
                        }
                        Err(e) => {
                            crate::metrics::record_tool_execution(
                                &tool_call.tool_name,
                                tool_start.elapsed().as_millis() as u64,
                                false,
                            );
                            self.audit_log(agentos_audit::AuditEntry {
                                timestamp: chrono::Utc::now(),
                                trace_id,
                                event_type: agentos_audit::AuditEventType::ToolExecutionFailed,
                                agent_id: Some(task.agent_id),
                                task_id: Some(task.id),
                                tool_id: None,
                                details: serde_json::json!({ "tool": tool_call.tool_name, "error": e.to_string() }),
                                severity: agentos_audit::AuditSeverity::Error,
                            });

                            let error_result = serde_json::json!({
                                "error": e.to_string()
                            });
                            self.context_manager
                                .push_tool_result(&task.id, &tool_call.tool_name, &error_result)
                                .await
                                .ok();

                            self.episodic_memory
                                .record(
                                    &task.id,
                                    &task.agent_id,
                                    agentos_memory::EpisodeType::ToolResult,
                                    &error_result.to_string(),
                                    Some(&format!("Tool '{}' failed: {}", tool_call.tool_name, e)),
                                    None,
                                    &trace_id,
                                )
                                .ok();
                        }
                    }
                }
                None => {
                    // No tool call — this is the final answer
                    final_answer = inference.text;
                    break;
                }
            }
        }

        self.context_manager.remove_context(&task.id).await;
        Ok(final_answer)
    }

    /// Execute a task from the background executor loop.
    pub(crate) async fn execute_task(&self, task: &AgentTask) {
        let start = std::time::Instant::now();
        crate::metrics::record_task_queued();

        self.scheduler
            .update_state(&task.id, TaskState::Running)
            .await
            .ok();

        match self.execute_task_sync(task).await {
            Ok(answer) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                tracing::info!(
                    "Task {} complete: {}",
                    task.id,
                    &answer[..answer.len().min(100)]
                );
                crate::metrics::record_task_completed(duration_ms, true);
                self.scheduler
                    .update_state(&task.id, TaskState::Complete)
                    .await
                    .ok();
                self.background_pool
                    .complete(&task.id, serde_json::json!({ "result": answer }))
                    .await;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                tracing::error!("Task {} failed: {}", task.id, e);
                crate::metrics::record_task_completed(duration_ms, false);
                self.scheduler
                    .update_state(&task.id, TaskState::Failed)
                    .await
                    .ok();
                self.background_pool.fail(&task.id, e.to_string()).await;
            }
        }
    }
}
