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

    pub(crate) async fn cmd_get_retrieval_metrics(&self) -> KernelResponse {
        let (refresh_total, reuse_total) = crate::metrics::retrieval_refresh_snapshot();
        let total_decisions = refresh_total + reuse_total;
        let refresh_ratio = if total_decisions == 0 {
            0.0
        } else {
            refresh_total as f64 / total_decisions as f64
        };
        let reuse_ratio = if total_decisions == 0 {
            0.0
        } else {
            reuse_total as f64 / total_decisions as f64
        };

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "refresh_total": refresh_total,
                "reuse_total": reuse_total,
                "total_decisions": total_decisions,
                "refresh_ratio": refresh_ratio,
                "reuse_ratio": reuse_ratio,
            })),
        }
    }
}
