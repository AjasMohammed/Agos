use std::sync::Arc;

use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_runtime::{ComputeRuntime, ContainerSpec, NetworkMode};
use agentos_types::{AgentID, TraceID};

use crate::Kernel;

/// Maximum memory per container (64 GiB).
const MAX_MEMORY_MB: u64 = 64 * 1024;
/// Maximum CPU per container (16 cores).
const MAX_CPU: f64 = 16.0;
/// Minimum exec timeout (1 second).
const MIN_TIMEOUT_MS: u64 = 1000;

impl Kernel {
    /// Helper: get the compute runtime or return an error response.
    fn require_runtime(&self) -> Result<Arc<dyn ComputeRuntime>, KernelResponse> {
        self.compute_runtime
            .as_ref()
            .map(Arc::clone)
            .ok_or(KernelResponse::Error {
                message: "Container runtime not available (Docker not connected)".into(),
            })
    }

    /// Helper: resolve agent name to AgentID or return an error response.
    async fn resolve_agent(&self, agent_name: &str) -> Result<AgentID, KernelResponse> {
        self.agent_registry
            .read()
            .await
            .get_by_name(agent_name)
            .map(|a| a.id)
            .ok_or(KernelResponse::Error {
                message: format!("Agent not found: {agent_name}"),
            })
    }

    /// Helper: verify the caller agent owns the given container.
    async fn verify_container_owner(
        &self,
        runtime: &dyn ComputeRuntime,
        container_id: &str,
        agent_id: AgentID,
    ) -> Result<(), KernelResponse> {
        let containers = runtime.list().await.map_err(|e| KernelResponse::Error {
            message: format!("Failed to list containers: {e}"),
        })?;
        match containers.iter().find(|c| c.id == container_id) {
            Some(c) if c.agent_id == agent_id => Ok(()),
            Some(_) => Err(KernelResponse::Error {
                message: "Access denied: container belongs to another agent".into(),
            }),
            None => Err(KernelResponse::Error {
                message: format!("Container not found: {container_id}"),
            }),
        }
    }

    /// Create a new container for an agent.
    pub(crate) async fn cmd_container_create(
        &self,
        agent_name: String,
        image: String,
        memory_mb: u64,
        cpu: f64,
        network: String,
        ttl_seconds: u64,
    ) -> KernelResponse {
        let runtime = match self.require_runtime() {
            Ok(rt) => rt,
            Err(e) => return e,
        };
        let agent_id = match self.resolve_agent(&agent_name).await {
            Ok(id) => id,
            Err(e) => return e,
        };

        // Input validation (I5, I6)
        if memory_mb == 0 || memory_mb > MAX_MEMORY_MB {
            return KernelResponse::Error {
                message: format!("memory_mb must be 1–{MAX_MEMORY_MB}"),
            };
        }
        if cpu <= 0.0 || cpu > MAX_CPU || cpu.is_nan() {
            return KernelResponse::Error {
                message: format!("cpu must be 0.1–{MAX_CPU}"),
            };
        }

        let network_mode = match network.as_str() {
            "none" | "" => NetworkMode::None,
            "outbound" => NetworkMode::Outbound,
            other => {
                return KernelResponse::Error {
                    message: format!("Invalid network mode: {other} (use 'none' or 'outbound')"),
                };
            }
        };

        let spec = ContainerSpec {
            image,
            memory_limit_bytes: memory_mb * 1024 * 1024,
            cpu_limit: cpu,
            pids_limit: 100,
            ttl_seconds,
            network: network_mode,
            env_vars: Default::default(),
            workspace_mount: None,
        };

        // Acquire provision lock to prevent TOCTOU races (C2)
        let _lock = self.quota_enforcer.provision_lock.lock().await;

        // Check quota
        let current = runtime.list().await.unwrap_or_default();
        if let Err(e) = self.quota_enforcer.check(&agent_id, &spec, &current).await {
            self.audit_container(
                AuditEventType::ContainerQuotaExceeded,
                Some(agent_id),
                serde_json::json!({
                    "reason": e.to_string(),
                    "image": &spec.image,
                }),
            );
            return KernelResponse::Error {
                message: e.to_string(),
            };
        }

        match runtime.provision(spec, agent_id, None).await {
            Ok(info) => {
                self.audit_container(
                    AuditEventType::ContainerProvisioned,
                    Some(agent_id),
                    serde_json::json!({
                        "container_id": &info.id,
                        "image": &info.image,
                        "agent": agent_name,
                    }),
                );
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "container_id": info.id,
                        "image": info.image,
                        "status": info.status,
                        "expires_at": info.expires_at.to_rfc3339(),
                    })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Container create failed: {e}"),
            },
        }
    }

    /// Execute a command in a running container (ownership verified).
    pub(crate) async fn cmd_container_exec(
        &self,
        agent_name: String,
        container_id: String,
        command: Vec<String>,
        timeout_ms: u64,
    ) -> KernelResponse {
        let runtime = match self.require_runtime() {
            Ok(rt) => rt,
            Err(e) => return e,
        };
        let agent_id = match self.resolve_agent(&agent_name).await {
            Ok(id) => id,
            Err(e) => return e,
        };

        // Ownership check (C1)
        if let Err(e) = self
            .verify_container_owner(runtime.as_ref(), &container_id, agent_id)
            .await
        {
            return e;
        }

        // Timeout validation (I4)
        if timeout_ms < MIN_TIMEOUT_MS {
            return KernelResponse::Error {
                message: format!("timeout_ms must be >= {MIN_TIMEOUT_MS}"),
            };
        }

        match runtime
            .exec(&container_id, command.clone(), timeout_ms)
            .await
        {
            Ok(result) => {
                self.audit_container(
                    AuditEventType::ContainerExecRun,
                    Some(agent_id),
                    serde_json::json!({
                        "container_id": &container_id,
                        "command": &command,
                        "agent": &agent_name,
                        "exit_code": result.exit_code,
                        "duration_ms": result.duration_ms,
                    }),
                );
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "exit_code": result.exit_code,
                        "stdout": result.stdout,
                        "stderr": result.stderr,
                        "duration_ms": result.duration_ms,
                    })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Container exec failed: {e}"),
            },
        }
    }

    /// Read logs from a container (ownership verified).
    pub(crate) async fn cmd_container_logs(
        &self,
        agent_name: String,
        container_id: String,
        tail: usize,
    ) -> KernelResponse {
        let runtime = match self.require_runtime() {
            Ok(rt) => rt,
            Err(e) => return e,
        };
        let agent_id = match self.resolve_agent(&agent_name).await {
            Ok(id) => id,
            Err(e) => return e,
        };

        // Ownership check (C1)
        if let Err(e) = self
            .verify_container_owner(runtime.as_ref(), &container_id, agent_id)
            .await
        {
            return e;
        }

        match runtime.logs(&container_id, tail).await {
            Ok(logs) => KernelResponse::Success {
                data: Some(serde_json::json!({ "logs": logs })),
            },
            Err(e) => KernelResponse::Error {
                message: format!("Container logs failed: {e}"),
            },
        }
    }

    /// Destroy a container (ownership verified).
    pub(crate) async fn cmd_container_destroy(
        &self,
        agent_name: String,
        container_id: String,
    ) -> KernelResponse {
        let runtime = match self.require_runtime() {
            Ok(rt) => rt,
            Err(e) => return e,
        };
        let agent_id = match self.resolve_agent(&agent_name).await {
            Ok(id) => id,
            Err(e) => return e,
        };

        // Ownership check (C1)
        if let Err(e) = self
            .verify_container_owner(runtime.as_ref(), &container_id, agent_id)
            .await
        {
            return e;
        }

        match runtime.destroy(&container_id).await {
            Ok(()) => {
                self.audit_container(
                    AuditEventType::ContainerDestroyed,
                    Some(agent_id),
                    serde_json::json!({
                        "container_id": &container_id,
                        "agent": &agent_name,
                    }),
                );
                KernelResponse::Success {
                    data: Some(serde_json::json!({ "destroyed": true })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Container destroy failed: {e}"),
            },
        }
    }

    /// List containers (optionally filtered by agent).
    pub(crate) async fn cmd_container_list(&self, agent_name: Option<String>) -> KernelResponse {
        let runtime = match self.require_runtime() {
            Ok(rt) => rt,
            Err(_) => {
                return KernelResponse::Success {
                    data: Some(serde_json::json!({ "containers": [], "runtime": "disabled" })),
                };
            }
        };

        let containers = match runtime.list().await {
            Ok(c) => c,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Container list failed: {e}"),
                };
            }
        };

        // Optional agent filter
        let filtered: Vec<_> = if let Some(ref name) = agent_name {
            let agent_id = self
                .agent_registry
                .read()
                .await
                .get_by_name(name)
                .map(|a| a.id);
            match agent_id {
                Some(id) => containers
                    .into_iter()
                    .filter(|c| c.agent_id == id)
                    .collect(),
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent not found: {name}"),
                    };
                }
            }
        } else {
            containers
        };

        let data: Vec<serde_json::Value> = filtered
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "image": c.image,
                    "status": c.status,
                    "agent_id": c.agent_id.to_string(),
                    "created_at": c.created_at.to_rfc3339(),
                    "expires_at": c.expires_at.to_rfc3339(),
                    "memory_mb": c.memory_limit_bytes / (1024 * 1024),
                    "cpu": c.cpu_limit,
                })
            })
            .collect();

        KernelResponse::Success {
            data: Some(serde_json::json!({ "containers": data })),
        }
    }

    /// Write a container audit log entry.
    fn audit_container(
        &self,
        event_type: AuditEventType,
        agent_id: Option<AgentID>,
        details: serde_json::Value,
    ) {
        if let Err(e) = self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type,
            agent_id,
            task_id: None,
            tool_id: None,
            details,
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        }) {
            tracing::error!(error = %e, "Failed to write container audit entry");
        }
    }
}
