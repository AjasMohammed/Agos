use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::TeamConfig;

impl Kernel {
    /// Execute a named agent team against a goal.
    ///
    /// Spawns the coordinator agent with a prompt that lists available workers.
    /// The coordinator uses `spawn_agent` and `await_agents` tools to delegate and
    /// aggregate work. Workers are spawned dynamically at runtime.
    pub(crate) async fn cmd_run_team(&self, config_json: &str) -> KernelResponse {
        let config: TeamConfig = match serde_json::from_str(config_json) {
            Ok(c) => c,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("invalid team config: {}", e),
                }
            }
        };

        let coordinator = match config.coordinator() {
            Some(c) => c.clone(),
            None => {
                return KernelResponse::Error {
                    message: "team has no coordinator member".to_string(),
                }
            }
        };

        let worker_names: Vec<String> = config
            .workers()
            .iter()
            .map(|w| w.agent_name.clone())
            .collect();

        let coordinator_prompt = format!(
            "You are the coordinator for team '{name}'. \
             Goal: {goal}\n\n\
             Available workers: {workers}\n\
             Use spawn_agent to delegate subtasks to workers. \
             Use await_agents to collect their results. \
             Produce a final consolidated response when all subtasks are complete.",
            name = config.name,
            goal = config.goal,
            workers = worker_names.join(", "),
        );

        // Resolve a root agent to act as the synthetic "parent" for the coordinator.
        // We pick the first available agent; cmd_spawn_sub_agent requires a real parent task,
        // so we spawn the coordinator as a top-level task via RunTask instead.
        let resp = self
            .cmd_run_task(
                Some(coordinator.agent_name.clone()),
                coordinator_prompt,
                false,
            )
            .await;

        match resp {
            KernelResponse::Success { data } => {
                // Extract the task ID from the success data if present.
                let coordinator_task_id = data
                    .as_ref()
                    .and_then(|d| d.get("task_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<agentos_types::TaskID>().ok())
                    .unwrap_or_else(agentos_types::TaskID::new);

                KernelResponse::TeamStarted {
                    coordinator_task_id,
                    worker_task_ids: vec![],
                }
            }
            other => other,
        }
    }
}
