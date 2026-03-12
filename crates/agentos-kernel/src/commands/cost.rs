use crate::kernel::Kernel;
use agentos_bus::KernelResponse;

impl Kernel {
    pub(crate) async fn cmd_get_cost_report(&self, agent_name: Option<String>) -> KernelResponse {
        match agent_name {
            Some(name) => {
                // Look up agent by name
                let registry = self.agent_registry.read().await;
                let agent = registry.get_by_name(&name);
                match agent {
                    Some(profile) => {
                        let agent_id = profile.id;
                        drop(registry);
                        match self.cost_tracker.get_snapshot(&agent_id).await {
                            Some(snap) => KernelResponse::CostReport(vec![snap]),
                            None => KernelResponse::CostReport(vec![]),
                        }
                    }
                    None => KernelResponse::Error {
                        message: format!("Agent '{}' not found", name),
                    },
                }
            }
            None => {
                // All agents
                let snapshots = self.cost_tracker.get_all_snapshots().await;
                KernelResponse::CostReport(snapshots)
            }
        }
    }
}
