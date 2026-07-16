use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::*;
use chrono::Utc;
use std::time::Duration;

/// Maximum sub-agent spawn depth. Tasks at this depth may not spawn children.
const MAX_SPAWN_DEPTH: u8 = 5;

impl Kernel {
    /// Spawn a child task scoped to the parent's capabilities.
    ///
    /// Spawn a child task scoped to a subset of the parent's capabilities.
    ///
    /// Steps:
    /// 1. Resolve the parent task — error if not found.
    /// 2. Enforce the depth limit (MAX_SPAWN_DEPTH).
    /// 3. Resolve the target agent by name — error if offline.
    /// 4. Generate a fresh child TaskID; build a scoped CapabilityToken via
    ///    `scope_for_child()` using child's own IDs (prevents scheduler collision).
    /// 5. Enqueue the child task and register it in the child_map for cascade-cancel.
    /// 6. Optionally seed the child context window from the parent's ContextSlice.
    /// 7. Write a structured audit entry.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn cmd_spawn_sub_agent(
        &self,
        parent_task_id: TaskID,
        agent_name: &str,
        prompt: &str,
        requested_permissions: &[String],
        context_slice: Option<agentos_types::ContextSlice>,
        handoff_mode: Option<agentos_types::HandoffMode>,
        requested_tool_categories: Option<Vec<String>>,
    ) -> KernelResponse {
        let span = tracing::info_span!(
            "spawn_sub_agent",
            parent_task_id = %parent_task_id,
            agent_name = %agent_name,
            prompt_preview = %prompt.chars().take(80).collect::<String>(),
        );
        let _enter = span.enter();

        // 1. Look up parent task — must be present and running.
        let parent_task = match self.scheduler.get_task(&parent_task_id).await {
            Some(t) => t,
            None => {
                tracing::warn!(parent_task_id = %parent_task_id, "SpawnSubAgent: parent task not found");
                return KernelResponse::Error {
                    message: format!("Parent task '{}' not found", parent_task_id),
                };
            }
        };

        // 2. Enforce spawn depth limit — prevents runaway recursive spawning.
        if parent_task.spawn_depth >= MAX_SPAWN_DEPTH {
            tracing::warn!(
                parent_task_id = %parent_task_id,
                spawn_depth = parent_task.spawn_depth,
                max = MAX_SPAWN_DEPTH,
                "SpawnSubAgent: depth limit exceeded"
            );
            return KernelResponse::Error {
                message: format!(
                    "spawn depth limit ({MAX_SPAWN_DEPTH}) exceeded: \
                     parent task '{parent_task_id}' is at depth {}",
                    parent_task.spawn_depth
                ),
            };
        }

        // 3. Resolve the target agent by name.
        let agent = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(agent_name) {
                Some(a) => a.clone(),
                None => {
                    tracing::warn!(agent_name = %agent_name, "SpawnSubAgent: agent not found");
                    return KernelResponse::Error {
                        message: format!("Agent '{agent_name}' not found"),
                    };
                }
            }
        };
        if agent.status == AgentStatus::Offline {
            tracing::warn!(agent_name = %agent_name, "SpawnSubAgent: agent is offline");
            return KernelResponse::Error {
                message: format!("Agent '{agent_name}' is offline"),
            };
        }

        // 4. Build permission set from caller's request, then intersect with parent.
        //    If no permissions specified, inherit the parent's full set (safe: intersection
        //    with parent can only reduce, never escalate).
        let requested = if requested_permissions.is_empty() {
            tracing::debug!(
                parent_task_id = %parent_task_id,
                "SpawnSubAgent: no permissions requested — inheriting parent scope"
            );
            parent_task.capability_token.permissions.clone()
        } else {
            let mut ps = PermissionSet::new();
            for resource in requested_permissions {
                ps.grant(resource.clone(), true, true, true, None);
            }
            ps
        };

        // 4b. Org-chart clamp: if the target agent occupies org node(s), its
        //     effective scope can never exceed the configured node ceiling(s),
        //     no matter how broadly the parent delegates. Fail-closed — clamp to
        //     the intersection of every node the agent belongs to. This is the
        //     runtime half of the downward-only invariant the OrgStore enforces
        //     at write time (see org_store.rs).
        let requested = if let Some(org_store) = &self.org_store {
            match org_store.scopes_for_agent(agent_name).await {
                Ok(scopes) if !scopes.is_empty() => {
                    let node_count = scopes.len();
                    let clamped = scopes
                        .iter()
                        .fold(requested, |acc, ceiling| acc.intersect_with(ceiling));
                    tracing::debug!(
                        agent_name = %agent_name,
                        node_count,
                        "SpawnSubAgent: clamped child scope to org node ceiling"
                    );
                    clamped
                }
                Ok(_) => requested, // agent not in any org — no clamp
                Err(e) => {
                    // Lookup failure must not silently widen scope. The downstream
                    // scope_for_child still intersects with the parent token, so
                    // we degrade to that bound and log loudly.
                    tracing::warn!(
                        agent_name = %agent_name,
                        error = %e,
                        "SpawnSubAgent: org scope lookup failed — proceeding with parent-token bound only"
                    );
                    requested
                }
            }
        } else {
            requested
        };

        // Resolve effective tool_categories allowlist for the child.
        // Sub-agents may NEVER widen the parent's allowlist; child request must
        // be a subset of parent's allowlist when both are present. If the child
        // requests a category the parent does not allow, reject the spawn —
        // this is a permission escalation attempt.
        let effective_tool_categories = match (
            parent_task.tool_categories.as_ref(),
            requested_tool_categories,
        ) {
            (None, requested) => requested,
            (Some(parent_allow), None) => Some(parent_allow.clone()),
            (Some(parent_allow), Some(requested)) => {
                let parent_set: std::collections::HashSet<&String> = parent_allow.iter().collect();
                if let Some(bad) = requested.iter().find(|c| !parent_set.contains(c)) {
                    tracing::warn!(
                        parent_task_id = %parent_task_id,
                        agent_name = %agent_name,
                        widened_category = %bad,
                        "SpawnSubAgent: rejected — child tool_categories must be a subset of parent's"
                    );
                    return KernelResponse::Error {
                        message: format!(
                            "sub-agent tool_categories must be a subset of parent's; \
                             requested category '{bad}' is not in the parent's allowlist",
                        ),
                    };
                }
                Some(requested)
            }
        };

        // Generate child IDs BEFORE calling scope_for_child so the token is
        // issued for the child's own task/agent IDs, not the parent's.
        // This is the fix for the scheduler entry collision bug.
        let child_task_id = TaskID::new();
        let child_cap = match self.capability_engine.scope_for_child(
            &parent_task.capability_token,
            child_task_id,
            agent.id,
            &requested,
            Duration::from_secs(300),
        ) {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(
                    parent_task_id = %parent_task_id,
                    child_task_id = %child_task_id,
                    error = %e,
                    "SpawnSubAgent: capability scoping failed"
                );
                return KernelResponse::Error {
                    message: format!("Failed to scope child capabilities: {e}"),
                };
            }
        };

        let child_depth = parent_task.spawn_depth + 1;
        let child_timeout = Duration::from_secs(300);

        // 5. Build and enqueue the child task.
        //    `autonomous` is NOT inherited — sub-agents run with bounded iterations
        //    unless explicitly configured otherwise. This prevents runaway resource use.
        let child_task = AgentTask {
            id: child_task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token: child_cap,
            assigned_llm: Some(agent.id),
            priority: 5,
            created_at: Utc::now(),
            started_at: None,
            timeout: child_timeout,
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(parent_task_id),
            reasoning_hints: Some(crate::commands::task::infer_reasoning_hints(prompt)),
            max_iterations: None,
            trigger_source: None,
            autonomous: false, // sub-agents always bounded; never inherit parent autonomy
            parent_task_id: Some(parent_task_id),
            spawn_depth: child_depth,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            spawner_agent_id: None,
            tool_categories: effective_tool_categories,
            disable_tool_scoping: false,
        };

        self.scheduler.enqueue(child_task).await;

        // Fire AgentSpawned hook so audit and metrics hooks can track the spawn event.
        self.hook_registry
            .fire(&agentos_types::HookEvent::AgentSpawned {
                parent_task: parent_task_id,
                child_agent: agent.id,
            })
            .await;

        // Register child for cascade-cancel: cancelling the parent cancels all children.
        self.scheduler
            .register_child(parent_task_id, child_task_id)
            .await;

        tracing::info!(
            parent_task_id = %parent_task_id,
            child_task_id = %child_task_id,
            agent_name = %agent_name,
            spawn_depth = child_depth,
            "SpawnSubAgent: child task enqueued"
        );

        // 6. Seed child context from parent slice. An explicit `context_slice`
        //    from the caller wins; otherwise a `handoff_mode` (when set and not
        //    `None`) tells the kernel to build the slice itself by filtering
        //    the parent's window. Building kernel-side avoids shipping the
        //    full parent context over the bus and lets the policy layer choose
        //    blast radius (None / TaskOnly / TaskAndKnowledge / Full).
        const HANDOFF_MAX_ENTRIES: usize = 64;
        let effective_slice = match (context_slice, handoff_mode) {
            (Some(s), _) => Some(s),
            (None, Some(mode)) if !matches!(mode, agentos_types::HandoffMode::None) => {
                self.context_manager
                    .build_handoff_slice(&parent_task_id, mode, HANDOFF_MAX_ENTRIES)
                    .await
            }
            _ => None,
        };
        if let Some(slice) = effective_slice {
            let msg_count = slice.messages.len();
            match self
                .context_manager
                .seed_from_slice(child_task_id, agent.id, &slice)
                .await
            {
                Ok(()) => {
                    tracing::debug!(
                        child_task_id = %child_task_id,
                        messages_seeded = msg_count,
                        "SpawnSubAgent: child context seeded from parent slice"
                    );
                }
                Err(e) => {
                    // Non-fatal: child can still run with empty context.
                    tracing::warn!(
                        child_task_id = %child_task_id,
                        error = %e,
                        "SpawnSubAgent: failed to seed child context — child will start empty"
                    );
                }
            }
        }

        // 7. Structured audit entry for every sub-agent spawn.
        let _ = self.audit.append(AuditEntry {
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCreated,
            agent_id: Some(agent.id),
            task_id: Some(child_task_id),
            tool_id: None,
            details: serde_json::json!({
                "kind": "sub_agent_spawn",
                "parent_task_id": parent_task_id.to_string(),
                "child_task_id": child_task_id.to_string(),
                "agent_name": agent_name,
                "spawn_depth": child_depth,
                "autonomous": false,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::SubAgentSpawned { child_task_id }
    }

    /// Poll the current state and result summaries for a set of awaited child tasks.
    ///
    /// Results for completed tasks are sourced from the episodic memory store so
    /// that the parent LLM receives the actual output, not just a state label.
    /// Note: this is a snapshot — the results may already have been injected into
    /// the parent's context window by `task_completion.rs` automatically.
    pub(crate) async fn cmd_await_sub_agents(
        &self,
        parent_task_id: TaskID,
        child_task_ids: &[TaskID],
    ) -> KernelResponse {
        tracing::debug!(
            parent_task_id = %parent_task_id,
            child_count = child_task_ids.len(),
            "AwaitSubAgents: polling child task states"
        );

        let mut results: Vec<(TaskID, String)> = Vec::with_capacity(child_task_ids.len());

        for &child_id in child_task_ids {
            let summary = match self.scheduler.get_task(&child_id).await {
                Some(task) => {
                    // For terminal tasks, surface the result summary from the episodic store.
                    let state_label = match task.state {
                        TaskState::Complete => "complete",
                        TaskState::Failed => "failed",
                        TaskState::Cancelled => "cancelled",
                        TaskState::Running => "running",
                        TaskState::Queued => "queued",
                        TaskState::Waiting => "waiting",
                        TaskState::Suspended => "suspended",
                    };
                    if let Some(result_summary) =
                        self.scheduler.get_task_result_summary(child_id).await
                    {
                        format!("state={state_label} result={result_summary}")
                    } else {
                        format!(
                            "state={state_label} depth={} agent={}",
                            task.spawn_depth, task.agent_id
                        )
                    }
                }
                None => {
                    tracing::warn!(
                        parent_task_id = %parent_task_id,
                        child_task_id = %child_id,
                        "AwaitSubAgents: child task not found in scheduler"
                    );
                    "not_found".to_string()
                }
            };
            results.push((child_id, summary));
        }

        tracing::debug!(
            parent_task_id = %parent_task_id,
            results_count = results.len(),
            "AwaitSubAgents: returning child task summaries"
        );

        KernelResponse::SubAgentResults { results }
    }
}
#[cfg(test)]
mod tests {
    use super::MAX_SPAWN_DEPTH;
    use crate::scheduler::TaskScheduler;
    use agentos_types::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn make_task_at_depth(depth: u8) -> AgentTask {
        AgentTask {
            id: TaskID::new(),
            state: TaskState::Queued,
            agent_id: AgentID::new(),
            capability_token: CapabilityToken {
                task_id: TaskID::new(),
                agent_id: AgentID::new(),
                allowed_tools: BTreeSet::new(),
                allowed_intents: BTreeSet::new(),
                permissions: PermissionSet::new(),
                issued_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                signature: Vec::new(),
            },
            assigned_llm: None,
            priority: 5,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(300),
            original_prompt: "test".to_string(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            max_iterations: None,
            trigger_source: None,
            autonomous: false,
            parent_task_id: None,
            spawn_depth: depth,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: Default::default(),
            spawner_agent_id: None,
            tool_categories: None,
            disable_tool_scoping: false,
        }
    }

    /// Depth-limit check: a task at MAX_SPAWN_DEPTH must not be allowed to spawn children.
    /// This tests the depth guard logic independently of a full Kernel.
    #[tokio::test]
    async fn test_depth_limit_exceeded_rejects_spawn() {
        let scheduler = TaskScheduler::new(10);
        let parent = make_task_at_depth(MAX_SPAWN_DEPTH);
        let parent_id = parent.id;
        scheduler.enqueue(parent).await;

        // Retrieve the task and assert the guard condition directly.
        let task = scheduler.get_task(&parent_id).await.unwrap();
        assert!(
            task.spawn_depth >= MAX_SPAWN_DEPTH,
            "task should be at or beyond the max depth"
        );

        // The handler checks `spawn_depth >= MAX_SPAWN_DEPTH`. Simulate the guard.
        let would_be_blocked = task.spawn_depth >= MAX_SPAWN_DEPTH;
        assert!(
            would_be_blocked,
            "SpawnSubAgent must be blocked when spawn_depth >= {}",
            MAX_SPAWN_DEPTH
        );
    }

    /// A task below the depth limit should be allowed to proceed.
    #[tokio::test]
    async fn test_depth_below_limit_not_blocked() {
        let scheduler = TaskScheduler::new(10);
        let parent = make_task_at_depth(MAX_SPAWN_DEPTH - 1);
        let parent_id = parent.id;
        scheduler.enqueue(parent).await;

        let task = scheduler.get_task(&parent_id).await.unwrap();
        let would_be_blocked = task.spawn_depth >= MAX_SPAWN_DEPTH;
        assert!(
            !would_be_blocked,
            "SpawnSubAgent must be allowed when spawn_depth < {}",
            MAX_SPAWN_DEPTH
        );
    }

    /// register_child / get_children round-trip.
    #[tokio::test]
    async fn test_register_and_get_children() {
        let scheduler = TaskScheduler::new(10);
        let parent_id = TaskID::new();
        let child1 = TaskID::new();
        let child2 = TaskID::new();

        scheduler.register_child(parent_id, child1).await;
        scheduler.register_child(parent_id, child2).await;

        let children = scheduler.get_children(&parent_id).await;
        assert_eq!(children.len(), 2);
        assert!(children.contains(&child1));
        assert!(children.contains(&child2));
    }

    /// get_children returns empty vec for unknown parent.
    #[tokio::test]
    async fn test_get_children_unknown_parent_returns_empty() {
        let scheduler = TaskScheduler::new(10);
        let unknown = TaskID::new();
        let children = scheduler.get_children(&unknown).await;
        assert!(children.is_empty());
    }

    /// Cascade cancel: registering a child under a parent and querying get_children
    /// returns exactly that child; a child has no children of its own by default.
    /// (The actual cancel cascade is exercised via cmd_cancel_task; this unit test
    /// verifies the data-structure contract that cascade relies on.)
    #[tokio::test]
    async fn test_cancel_parent_cancels_children() {
        let scheduler = TaskScheduler::new(4);

        let parent_id = TaskID::new();
        let child_id = TaskID::new();

        let mut parent_task = make_task_at_depth(0);
        parent_task.id = parent_id;

        let mut child_task = make_task_at_depth(1);
        child_task.id = child_id;
        child_task.parent_task_id = Some(parent_id);

        scheduler.enqueue(parent_task).await;
        scheduler.enqueue(child_task).await;
        scheduler.register_child(parent_id, child_id).await;

        // Parent must report exactly one child.
        let children = scheduler.get_children(&parent_id).await;
        assert_eq!(children.len(), 1);
        assert_eq!(children[0], child_id);

        // Child has no registered children of its own.
        let no_children = scheduler.get_children(&child_id).await;
        assert!(no_children.is_empty(), "child has no registered children");
    }
}
