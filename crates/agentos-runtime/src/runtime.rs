use agentos_types::{AgentID, AgentOSError, TaskID};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Specification for provisioning a container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSpec {
    /// Docker image to use (e.g. "python:3.11-slim", "node:20-alpine")
    pub image: String,
    /// Memory limit in bytes (default: 1 GiB)
    pub memory_limit_bytes: u64,
    /// CPU limit as fraction of cores (default: 1.0)
    pub cpu_limit: f64,
    /// Maximum number of PIDs (default: 100)
    pub pids_limit: i64,
    /// Time-to-live in seconds (default: 3600 = 1 hour)
    pub ttl_seconds: u64,
    /// Network isolation mode
    pub network: NetworkMode,
    /// Environment variables to set
    pub env_vars: HashMap<String, String>,
    /// Host directory to mount as /workspace in the container
    pub workspace_mount: Option<PathBuf>,
}

impl Default for ContainerSpec {
    fn default() -> Self {
        Self {
            image: "alpine:3.19".into(),
            memory_limit_bytes: 1024 * 1024 * 1024, // 1 GiB
            cpu_limit: 1.0,
            pids_limit: 100,
            ttl_seconds: 3600,
            network: NetworkMode::None,
            env_vars: HashMap::new(),
            workspace_mount: None,
        }
    }
}

/// Network isolation mode for containers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// No network access (default — most secure)
    None,
    /// Outbound internet only, no inbound connections
    Outbound,
}

/// Information about a running or stopped container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: String,
    pub image: String,
    pub status: ContainerStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub agent_id: AgentID,
    pub task_id: Option<TaskID>,
    /// Memory limit from the spec (bytes), for quota tracking.
    pub memory_limit_bytes: u64,
    /// CPU limit from the spec (core fractions), for quota tracking.
    pub cpu_limit: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerStatus {
    Creating,
    Running,
    Stopped,
    Failed(String),
}

/// Result of executing a command inside a container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

/// Pluggable backend for container/VM orchestration.
#[async_trait]
pub trait ComputeRuntime: Send + Sync {
    /// Provision a new container from the spec.
    async fn provision(
        &self,
        spec: ContainerSpec,
        agent_id: AgentID,
        task_id: Option<TaskID>,
    ) -> Result<ContainerInfo, AgentOSError>;

    /// Execute a command inside a running container.
    async fn exec(
        &self,
        container_id: &str,
        command: Vec<String>,
        timeout_ms: u64,
    ) -> Result<ExecResult, AgentOSError>;

    /// Read stdout/stderr logs from a container.
    async fn logs(&self, container_id: &str, tail: usize) -> Result<String, AgentOSError>;

    /// Destroy a container (force-kill if running).
    async fn destroy(&self, container_id: &str) -> Result<(), AgentOSError>;

    /// List all managed containers.
    async fn list(&self) -> Result<Vec<ContainerInfo>, AgentOSError>;

    /// Health check the runtime backend (e.g., Docker daemon is reachable).
    async fn health_check(&self) -> Result<bool, AgentOSError>;
}
