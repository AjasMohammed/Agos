use std::sync::Arc;

use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_runtime::{ComputeRuntime, ContainerSpec, NetworkMode};
use agentos_types::{AgentID, AgentTask, TraceID};

use crate::kernel_action::KernelActionResult;
use crate::Kernel;

fn action_error(message: String) -> KernelActionResult {
    KernelActionResult {
        success: false,
        result: serde_json::json!({ "error": message }),
    }
}

/// Maximum memory per container (64 GiB).
const MAX_MEMORY_MB: u64 = 64 * 1024;
/// Maximum CPU per container (16 cores).
const MAX_CPU: f64 = 16.0;
/// Minimum exec timeout (1 second).
const MIN_TIMEOUT_MS: u64 = 1000;
/// Maximum exec timeout (10 minutes). The kernel action has no outer timeout,
/// so this bounds how long one call can hold the agent's turn.
const MAX_TIMEOUT_MS: u64 = 600_000;
/// Maximum container lifetime (24 hours). Also keeps `expires_at` arithmetic
/// far from chrono's overflow panic.
const MAX_TTL_SECONDS: u64 = 24 * 3600;

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
        if ttl_seconds == 0 || ttl_seconds > MAX_TTL_SECONDS {
            return KernelResponse::Error {
                message: format!("ttl_seconds must be 1–{MAX_TTL_SECONDS}"),
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

        // Pull outside the provision lock: a multi-GB image must not block
        // every other agent's create.
        if let Err(e) = runtime.prepare_image(&spec.image).await {
            return KernelResponse::Error {
                message: format!("Container create failed: {e}"),
            };
        }

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
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
            return KernelResponse::Error {
                message: format!("timeout_ms must be {MIN_TIMEOUT_MS}–{MAX_TIMEOUT_MS}"),
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

    /// Run a `container-*` tool call as the calling agent. The tool is a
    /// stateless shim; everything is enforced by the `cmd_container_*`
    /// handlers above, the same ones the bus commands use.
    pub(crate) async fn execute_container_action(
        &self,
        task: &AgentTask,
        tool: &str,
        payload: &serde_json::Value,
    ) -> KernelActionResult {
        let Some(agent_name) = self
            .agent_registry
            .read()
            .await
            .get_by_id(&task.agent_id)
            .map(|a| a.name.clone())
        else {
            return action_error(format!("Agent not found: {}", task.agent_id));
        };
        let str_field = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let u64_field = |k: &str, d: u64| payload.get(k).and_then(|v| v.as_u64()).unwrap_or(d);
        let container_id = str_field("container_id");
        let missing_id = || action_error(format!("{tool} requires 'container_id'"));

        let response = match tool {
            "container-create" => {
                let Some(image) = str_field("image") else {
                    return action_error("container-create requires 'image'".into());
                };
                self.cmd_container_create(
                    agent_name,
                    image,
                    u64_field("memory_mb", 256),
                    payload.get("cpu").and_then(|v| v.as_f64()).unwrap_or(1.0),
                    str_field("network").unwrap_or_default(),
                    u64_field("ttl_seconds", 3600),
                )
                .await
            }
            "container-exec" => {
                let Some(container_id) = container_id else {
                    return missing_id();
                };
                let command: Option<Vec<String>> = payload
                    .get("command")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect());
                let Some(command) = command.filter(|c| !c.is_empty()) else {
                    return action_error(
                        "container-exec requires 'command' as a non-empty array of strings".into(),
                    );
                };
                self.cmd_container_exec(
                    agent_name,
                    container_id,
                    command,
                    u64_field("timeout_ms", 60_000),
                )
                .await
            }
            "container-logs" => {
                let Some(container_id) = container_id else {
                    return missing_id();
                };
                self.cmd_container_logs(agent_name, container_id, u64_field("tail", 200) as usize)
                    .await
            }
            "container-destroy" => {
                let Some(container_id) = container_id else {
                    return missing_id();
                };
                self.cmd_container_destroy(agent_name, container_id).await
            }
            // Always scoped to the caller: an agent never lists other agents'
            // containers.
            "container-list" => self.cmd_container_list(Some(agent_name)).await,
            other => return action_error(format!("Unknown container tool: {other}")),
        };

        match response {
            KernelResponse::Success { data } => KernelActionResult {
                success: true,
                result: data.unwrap_or_default(),
            },
            KernelResponse::Error { message } => action_error(message),
            other => action_error(format!("Unexpected container response: {other:?}")),
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
