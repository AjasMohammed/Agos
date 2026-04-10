use crate::runtime::{
    ComputeRuntime, ContainerInfo, ContainerSpec, ContainerStatus, ExecResult, NetworkMode,
};
use agentos_types::{AgentID, AgentOSError, TaskID};
use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, LogsOptions, RemoveContainerOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::HostConfig;
use bollard::Docker;
use chrono::{Duration, Utc};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

const LABEL_PREFIX: &str = "agentos";
/// Maximum stdout/stderr capture per exec call (1 MiB).
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

fn runtime_err(reason: impl std::fmt::Display) -> AgentOSError {
    AgentOSError::ToolExecutionFailed {
        tool_name: "container-runtime".into(),
        reason: reason.to_string(),
    }
}

/// Docker-based implementation of `ComputeRuntime`.
pub struct DockerRuntime {
    client: Docker,
    containers: Arc<RwLock<HashMap<String, ContainerInfo>>>,
    allowlist: crate::allowlist::ImageAllowlist,
}

impl DockerRuntime {
    /// Connect to the Docker daemon using default socket detection.
    pub async fn new(allowed_images: Vec<String>) -> Result<Self, AgentOSError> {
        let client = Docker::connect_with_socket_defaults()
            .map_err(|e| runtime_err(format!("Docker connection failed: {e}")))?;

        // Verify connectivity
        client
            .ping()
            .await
            .map_err(|e| runtime_err(format!("Docker daemon unreachable: {e}")))?;

        Ok(Self {
            client,
            containers: Arc::new(RwLock::new(HashMap::new())),
            allowlist: crate::allowlist::ImageAllowlist::new(allowed_images),
        })
    }
}

#[async_trait]
impl ComputeRuntime for DockerRuntime {
    async fn provision(
        &self,
        spec: ContainerSpec,
        agent_id: AgentID,
        task_id: Option<TaskID>,
    ) -> Result<ContainerInfo, AgentOSError> {
        // Enforce image allowlist — agents cannot use unapproved images
        if !self.allowlist.is_allowed(&spec.image) {
            return Err(runtime_err(format!(
                "Image '{}' is not in the operator allowlist",
                spec.image
            )));
        }

        let now = Utc::now();
        let expires_at = now + Duration::seconds(spec.ttl_seconds as i64);

        // Build labels for tracking managed containers
        let mut labels = HashMap::new();
        labels.insert(format!("{LABEL_PREFIX}.managed"), "true".to_string());
        labels.insert(format!("{LABEL_PREFIX}.agent_id"), agent_id.to_string());
        labels.insert(
            format!("{LABEL_PREFIX}.expires_at"),
            expires_at.to_rfc3339(),
        );
        if let Some(tid) = &task_id {
            labels.insert(format!("{LABEL_PREFIX}.task_id"), tid.to_string());
        }

        // Build volume mounts — block path traversal per project security rules
        let mut binds = Vec::new();
        if let Some(ref workspace) = spec.workspace_mount {
            let path_str = workspace.to_string_lossy();
            if path_str.contains("..") {
                return Err(runtime_err(
                    "Workspace path must not contain '..' (path traversal blocked)",
                ));
            }
            if !workspace.is_absolute() {
                return Err(runtime_err("Workspace path must be absolute"));
            }
            binds.push(format!("{}:/workspace", workspace.display()));
        }

        // Network mode
        let network_mode = match spec.network {
            NetworkMode::None => Some("none".to_string()),
            NetworkMode::Outbound => Some("bridge".to_string()),
        };

        // Environment variables
        let env: Vec<String> = spec
            .env_vars
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();

        // CPU quota: spec.cpu_limit * 100_000 (Docker CPU period is 100ms)
        let cpu_quota = (spec.cpu_limit * 100_000.0) as i64;

        let host_config = HostConfig {
            memory: Some(spec.memory_limit_bytes as i64),
            memory_swap: Some(spec.memory_limit_bytes as i64), // no swap
            cpu_quota: Some(cpu_quota),
            cpu_period: Some(100_000),
            pids_limit: Some(spec.pids_limit),
            binds: if binds.is_empty() { None } else { Some(binds) },
            network_mode,
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            cap_drop: Some(vec!["ALL".to_string()]),
            readonly_rootfs: Some(false), // /workspace + /tmp need writes
            ..Default::default()
        };

        let container_name = format!(
            "agentos-{}-{}",
            agent_id.to_string().split('-').next().unwrap_or("agent"),
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("0"),
        );

        let config = Config {
            image: Some(spec.image.clone()),
            env: if env.is_empty() { None } else { Some(env) },
            labels: Some(labels.into_iter().collect()),
            host_config: Some(host_config),
            // Keep the container alive with a sleep command
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            working_dir: Some("/workspace".to_string()),
            tty: Some(false),
            ..Default::default()
        };

        let create_opts = CreateContainerOptions {
            name: container_name.as_str(),
            platform: None,
        };

        // Create container
        let response = self
            .client
            .create_container(Some(create_opts), config)
            .await
            .map_err(|e| runtime_err(format!("Container create failed: {e}")))?;

        let container_id = response.id;

        // Start container
        self.client
            .start_container::<String>(&container_id, None)
            .await
            .map_err(|e| runtime_err(format!("Container start failed: {e}")))?;

        let info = ContainerInfo {
            id: container_id.clone(),
            memory_limit_bytes: spec.memory_limit_bytes,
            cpu_limit: spec.cpu_limit,
            image: spec.image,
            status: ContainerStatus::Running,
            created_at: now,
            expires_at,
            agent_id,
            task_id,
        };

        self.containers
            .write()
            .await
            .insert(container_id.clone(), info.clone());

        tracing::info!(
            container_id = %container_id,
            agent_id = %agent_id,
            image = %info.image,
            ttl = spec.ttl_seconds,
            "Container provisioned"
        );

        Ok(info)
    }

    async fn exec(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout_ms: u64,
    ) -> Result<ExecResult, AgentOSError> {
        let start = std::time::Instant::now();

        let exec_opts = CreateExecOptions {
            cmd: Some(command),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(false),
            ..Default::default()
        };

        let exec = self
            .client
            .create_exec(container_id, exec_opts)
            .await
            .map_err(|e| runtime_err(format!("Exec create failed: {e}")))?;

        let output = self
            .client
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| runtime_err(format!("Exec start failed: {e}")))?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached { mut output, .. } = output {
            let timeout = tokio::time::Duration::from_millis(timeout_ms);
            let deadline = tokio::time::Instant::now() + timeout;

            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {
                        break;
                    }
                    chunk = output.next() => {
                        match chunk {
                            Some(Ok(msg)) => {
                                let text = msg.to_string();
                                match msg {
                                    bollard::container::LogOutput::StdOut { .. } => {
                                        if stdout.len() + text.len() <= MAX_OUTPUT_BYTES {
                                            stdout.push_str(&text);
                                        }
                                    }
                                    bollard::container::LogOutput::StdErr { .. } => {
                                        if stderr.len() + text.len() <= MAX_OUTPUT_BYTES {
                                            stderr.push_str(&text);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Some(Err(e)) => {
                                tracing::warn!(error = %e, "Exec stream error");
                                break;
                            }
                            None => break,
                        }
                    }
                }
            }
        }

        // Get exit code
        let inspect = self
            .client
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| runtime_err(format!("Exec inspect failed: {e}")))?;

        let exit_code = inspect.exit_code.unwrap_or(-1);
        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(ExecResult {
            exit_code,
            stdout,
            stderr,
            duration_ms,
        })
    }

    async fn logs(&self, container_id: &str, tail: usize) -> Result<String, AgentOSError> {
        let opts = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            tail: tail.to_string(),
            ..Default::default()
        };

        let mut stream = self.client.logs(container_id, Some(opts));
        let mut output = String::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(msg) => {
                    output.push_str(&msg.to_string());
                    if output.len() > MAX_OUTPUT_BYTES {
                        output.truncate(MAX_OUTPUT_BYTES);
                        output.push_str("\n... [truncated]");
                        break;
                    }
                }
                Err(e) => {
                    return Err(runtime_err(format!("Log stream error: {e}")));
                }
            }
        }

        Ok(output)
    }

    async fn destroy(&self, container_id: &str) -> Result<(), AgentOSError> {
        // Stop with 10s grace period
        let stop_opts = StopContainerOptions { t: 10 };
        let _ = self
            .client
            .stop_container(container_id, Some(stop_opts))
            .await;

        // Force remove
        let rm_opts = RemoveContainerOptions {
            force: true,
            v: true, // remove associated volumes
            ..Default::default()
        };

        self.client
            .remove_container(container_id, Some(rm_opts))
            .await
            .map_err(|e| runtime_err(format!("Container remove failed: {e}")))?;

        self.containers.write().await.remove(container_id);

        tracing::info!(container_id = %container_id, "Container destroyed");
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ContainerInfo>, AgentOSError> {
        Ok(self.containers.read().await.values().cloned().collect())
    }

    async fn health_check(&self) -> Result<bool, AgentOSError> {
        match self.client.ping().await {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!(error = %e, "Docker health check failed");
                Ok(false)
            }
        }
    }
}
