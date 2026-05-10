use crate::runtime::{ContainerInfo, ContainerSpec};
use agentos_types::{AgentID, AgentOSError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Per-agent resource quota for container provisioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerQuota {
    /// Maximum concurrent containers per agent.
    pub max_containers: usize,
    /// Maximum total memory across all containers (bytes).
    pub max_total_memory_bytes: u64,
    /// Maximum total CPU across all containers (core fractions).
    pub max_total_cpu: f64,
}

impl Default for ContainerQuota {
    fn default() -> Self {
        Self {
            max_containers: 3,
            max_total_memory_bytes: 4 * 1024 * 1024 * 1024, // 4 GiB
            max_total_cpu: 4.0,
        }
    }
}

/// Enforces per-agent container quotas at provision time.
pub struct QuotaEnforcer {
    overrides: RwLock<HashMap<AgentID, ContainerQuota>>,
    default_quota: ContainerQuota,
    /// Serializes quota-check + provision to prevent TOCTOU races where
    /// two concurrent creates both pass the quota check.
    pub provision_lock: tokio::sync::Mutex<()>,
}

impl QuotaEnforcer {
    pub fn new(default_quota: ContainerQuota) -> Self {
        Self {
            overrides: RwLock::new(HashMap::new()),
            default_quota,
            provision_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Set a custom quota for a specific agent.
    pub async fn set_quota(&self, agent_id: AgentID, quota: ContainerQuota) {
        self.overrides.write().await.insert(agent_id, quota);
    }

    /// Get the effective quota for an agent (override or default).
    pub async fn get_quota(&self, agent_id: &AgentID) -> ContainerQuota {
        self.overrides
            .read()
            .await
            .get(agent_id)
            .cloned()
            .unwrap_or_else(|| self.default_quota.clone())
    }

    /// Check whether an agent can provision a container with the given spec,
    /// given their currently running containers.
    ///
    /// Returns `Ok(())` if within quota, or `Err(AgentOSError::QuotaExceeded)` otherwise.
    pub async fn check(
        &self,
        agent_id: &AgentID,
        spec: &ContainerSpec,
        current_containers: &[ContainerInfo],
    ) -> Result<(), AgentOSError> {
        let quota = self.get_quota(agent_id).await;

        // Filter to this agent's containers
        let agent_containers: Vec<&ContainerInfo> = current_containers
            .iter()
            .filter(|c| &c.agent_id == agent_id)
            .collect();

        // Check container count
        if agent_containers.len() >= quota.max_containers {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "container-create".into(),
                reason: format!(
                    "Container quota exceeded: agent has {}/{} containers",
                    agent_containers.len(),
                    quota.max_containers,
                ),
            });
        }

        // Check total memory
        let current_memory: u64 = agent_containers.iter().map(|c| c.memory_limit_bytes).sum();
        if current_memory + spec.memory_limit_bytes > quota.max_total_memory_bytes {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "container-create".into(),
                reason: format!(
                    "Memory quota exceeded: {:.0} MiB used + {:.0} MiB requested > {:.0} MiB limit",
                    current_memory as f64 / 1_048_576.0,
                    spec.memory_limit_bytes as f64 / 1_048_576.0,
                    quota.max_total_memory_bytes as f64 / 1_048_576.0,
                ),
            });
        }

        // Check total CPU
        let current_cpu: f64 = agent_containers.iter().map(|c| c.cpu_limit).sum();
        if current_cpu + spec.cpu_limit > quota.max_total_cpu {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "container-create".into(),
                reason: format!(
                    "CPU quota exceeded: {current_cpu:.1} cores used + {:.1} requested > {:.1} limit",
                    spec.cpu_limit, quota.max_total_cpu,
                ),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::NetworkMode;
    use chrono::Utc;

    fn make_info(agent_id: AgentID, memory: u64, cpu: f64) -> ContainerInfo {
        ContainerInfo {
            id: uuid::Uuid::new_v4().to_string(),
            image: "test:latest".into(),
            status: crate::runtime::ContainerStatus::Running,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            agent_id,
            task_id: None,
            memory_limit_bytes: memory,
            cpu_limit: cpu,
        }
    }

    fn make_spec(memory: u64, cpu: f64) -> ContainerSpec {
        ContainerSpec {
            image: "python:3.11-slim".into(),
            memory_limit_bytes: memory,
            cpu_limit: cpu,
            pids_limit: 100,
            ttl_seconds: 3600,
            network: NetworkMode::None,
            env_vars: Default::default(),
            workspace_mount: None,
        }
    }

    #[tokio::test]
    async fn test_within_quota() {
        let enforcer = QuotaEnforcer::new(ContainerQuota::default());
        let agent = AgentID::new();
        let spec = make_spec(1024 * 1024 * 1024, 1.0); // 1 GiB, 1 core
        let containers = vec![];
        assert!(enforcer.check(&agent, &spec, &containers).await.is_ok());
    }

    #[tokio::test]
    async fn test_container_count_exceeded() {
        let enforcer = QuotaEnforcer::new(ContainerQuota {
            max_containers: 2,
            ..Default::default()
        });
        let agent = AgentID::new();
        let containers = vec![
            make_info(agent, 512 * 1024 * 1024, 0.5),
            make_info(agent, 512 * 1024 * 1024, 0.5),
        ];
        let spec = make_spec(512 * 1024 * 1024, 0.5);
        assert!(enforcer.check(&agent, &spec, &containers).await.is_err());
    }

    #[tokio::test]
    async fn test_memory_exceeded() {
        let enforcer = QuotaEnforcer::new(ContainerQuota {
            max_total_memory_bytes: 2 * 1024 * 1024 * 1024, // 2 GiB
            ..Default::default()
        });
        let agent = AgentID::new();
        let containers = vec![make_info(
            agent,
            1536 * 1024 * 1024, // 1.5 GiB
            0.5,
        )];
        let spec = make_spec(1024 * 1024 * 1024, 0.5); // 1 GiB → total 2.5 > 2
        assert!(enforcer.check(&agent, &spec, &containers).await.is_err());
    }

    #[tokio::test]
    async fn test_cpu_exceeded() {
        let enforcer = QuotaEnforcer::new(ContainerQuota {
            max_total_cpu: 2.0,
            ..Default::default()
        });
        let agent = AgentID::new();
        let containers = vec![make_info(agent, 512 * 1024 * 1024, 1.5)];
        let spec = make_spec(512 * 1024 * 1024, 1.0); // total 2.5 > 2.0
        assert!(enforcer.check(&agent, &spec, &containers).await.is_err());
    }

    #[tokio::test]
    async fn test_other_agent_containers_not_counted() {
        let enforcer = QuotaEnforcer::new(ContainerQuota {
            max_containers: 2,
            ..Default::default()
        });
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        let containers = vec![
            make_info(agent_b, 1024 * 1024 * 1024, 1.0),
            make_info(agent_b, 1024 * 1024 * 1024, 1.0),
        ];
        let spec = make_spec(512 * 1024 * 1024, 0.5);
        // agent_a has 0 containers, so should be allowed
        assert!(enforcer.check(&agent_a, &spec, &containers).await.is_ok());
    }

    #[tokio::test]
    async fn test_custom_quota_override() {
        let enforcer = QuotaEnforcer::new(ContainerQuota {
            max_containers: 1,
            ..Default::default()
        });
        let agent = AgentID::new();

        // Override with higher limit
        enforcer
            .set_quota(
                agent,
                ContainerQuota {
                    max_containers: 5,
                    ..Default::default()
                },
            )
            .await;

        let containers = vec![
            make_info(agent, 512 * 1024 * 1024, 0.5),
            make_info(agent, 512 * 1024 * 1024, 0.5),
        ];
        let spec = make_spec(512 * 1024 * 1024, 0.5);
        assert!(enforcer.check(&agent, &spec, &containers).await.is_ok());
    }
}
