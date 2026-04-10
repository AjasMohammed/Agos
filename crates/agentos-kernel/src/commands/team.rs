use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::*;
use chrono::Utc;
use std::collections::BTreeSet;
use std::time::Duration;

impl Kernel {
    /// Execute an agent team against its declared goal.
    ///
    /// This is a **non-blocking** spawn: the coordinator task is enqueued on the
    /// background scheduler queue and `TeamStarted` is returned immediately with
    /// the coordinator's `TaskID`. The coordinator then uses `spawn_agent` and
    /// `await_agents` tools to delegate work to workers at runtime.
    ///
    /// The bus handler is never blocked for the duration of team execution.
    pub(crate) async fn cmd_run_team(&self, config_json: &str) -> KernelResponse {
        // --- Parse and validate the team config -----------------------------------

        let config: TeamConfig = match serde_json::from_str(config_json) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "RunTeam: failed to deserialize TeamConfig");
                return KernelResponse::Error {
                    message: format!("invalid team config: {e}"),
                };
            }
        };

        tracing::info!(
            team_name = %config.name,
            goal_preview = %config.goal.chars().take(120).collect::<String>(),
            member_count = config.members.len(),
            max_rounds = config.max_rounds,
            "RunTeam: starting agent team"
        );

        let coordinator = match config.coordinator() {
            Some(c) => c.clone(),
            None => {
                tracing::warn!(team_name = %config.name, "RunTeam: team has no coordinator member");
                return KernelResponse::Error {
                    message:
                        "team has no coordinator member — add a member with role = \"Coordinator\""
                            .to_string(),
                };
            }
        };

        // --- Resolve coordinator agent -------------------------------------------

        let agent = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(&coordinator.agent_name) {
                Some(a) => a.clone(),
                None => {
                    tracing::warn!(
                        team_name = %config.name,
                        coordinator_agent = %coordinator.agent_name,
                        "RunTeam: coordinator agent not registered"
                    );
                    return KernelResponse::Error {
                        message: format!(
                            "coordinator agent '{}' not registered",
                            coordinator.agent_name
                        ),
                    };
                }
            }
        };

        if agent.status == AgentStatus::Offline {
            tracing::warn!(
                team_name = %config.name,
                coordinator_agent = %coordinator.agent_name,
                "RunTeam: coordinator agent is offline"
            );
            return KernelResponse::Error {
                message: format!("coordinator agent '{}' is offline", coordinator.agent_name),
            };
        }

        // --- Build coordinator prompt --------------------------------------------

        let worker_roster: Vec<String> = config
            .workers()
            .iter()
            .map(|w| {
                if w.role_description.is_empty() {
                    w.agent_name.clone()
                } else {
                    format!("{} ({})", w.agent_name, w.role_description)
                }
            })
            .collect();

        let coordinator_prompt = format!(
            "You are the coordinator for team '{team_name}'. \
             Goal: {goal}\n\n\
             Available workers: {workers}\n\
             Max rounds: {max_rounds}\n\n\
             {role_description}\
             Use spawn_agent to delegate subtasks to workers. \
             Use await_agents to collect their results. \
             When all subtasks are complete, produce a final consolidated response.",
            team_name = config.name,
            goal = config.goal,
            workers = worker_roster.join(", "),
            max_rounds = config.max_rounds,
            role_description = if coordinator.role_description.is_empty() {
                String::new()
            } else {
                format!("Your role: {}\n\n", coordinator.role_description)
            },
        );

        // --- Issue capability token and build coordinator task -------------------

        let coordinator_task_id = TaskID::new();
        let task_timeout = Duration::from_secs(self.config.kernel.default_task_timeout_secs);

        let mut effective_permissions = {
            let registry = self.agent_registry.read().await;
            registry.compute_effective_permissions(&agent.id)
        };
        // Coordinator needs spawn permission to use spawn_agent tool.
        effective_permissions.grant("spawn".into(), false, false, true, None);

        let capability_token = match self.capability_engine.issue_token(
            coordinator_task_id,
            agent.id,
            BTreeSet::new(),
            BTreeSet::from([
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
            task_timeout,
        ) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(
                    team_name = %config.name,
                    coordinator_agent = %coordinator.agent_name,
                    error = %e,
                    "RunTeam: failed to issue coordinator capability token"
                );
                return KernelResponse::Error {
                    message: format!("Failed to issue coordinator token: {e}"),
                };
            }
        };

        // --- Enqueue coordinator task (non-blocking) -----------------------------

        let coordinator_task = AgentTask {
            id: coordinator_task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token,
            assigned_llm: Some(agent.id),
            priority: 5,
            created_at: Utc::now(),
            started_at: None,
            timeout: task_timeout,
            original_prompt: coordinator_prompt,
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            max_iterations: Some(config.max_rounds * 4), // each round ≈ 4 LLM iterations
            trigger_source: None,
            autonomous: false,
            parent_task_id: None,
            spawn_depth: 0,
            is_team_coordinator: true, // enables reliable filtering in `team list`
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
        };

        self.scheduler.enqueue(coordinator_task).await;

        tracing::info!(
            team_name = %config.name,
            coordinator_task_id = %coordinator_task_id,
            coordinator_agent = %coordinator.agent_name,
            worker_count = config.workers().len(),
            "RunTeam: coordinator task enqueued — team is running"
        );

        // --- Audit log -----------------------------------------------------------

        let _ = self.audit.append(AuditEntry {
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCreated,
            agent_id: Some(agent.id),
            task_id: Some(coordinator_task_id),
            tool_id: None,
            details: serde_json::json!({
                "kind": "team_run",
                "team_name": config.name,
                "goal_preview": config.goal.chars().take(200).collect::<String>(),
                "coordinator_agent": coordinator.agent_name,
                "workers": worker_roster,
                "max_rounds": config.max_rounds,
                "coordinator_task_id": coordinator_task_id.to_string(),
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::TeamStarted {
            coordinator_task_id,
            worker_task_ids: vec![], // workers are spawned dynamically by the coordinator
        }
    }
}
