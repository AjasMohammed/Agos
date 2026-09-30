use crate::runtime::{
    ComputeRuntime, ContainerInfo, ContainerSpec, ContainerStatus, ExecResult, NetworkMode,
};
use agentos_types::{AgentID, AgentOSError, TaskID};
use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogsOptions, RemoveContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
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
/// Upper bound on a single image pull.
const PULL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

fn runtime_err(reason: impl std::fmt::Display) -> AgentOSError {
    AgentOSError::ToolExecutionFailed {
        tool_name: "container-runtime".into(),
        reason: reason.to_string(),
    }
}

/// Docker-based implementation of `ComputeRuntime`.
pub struct DockerRuntime {
    client: Docker,
    /// Identifies the owning kernel (its data dir) so leftover cleanup never
    /// touches another kernel's containers on the same Docker host.
    instance: String,
    containers: Arc<RwLock<HashMap<String, ContainerInfo>>>,
    allowlist: crate::allowlist::ImageAllowlist,
}

impl DockerRuntime {
    /// Connect to the Docker daemon using default socket detection.
    ///
    /// `instance` must be stable across restarts of the same kernel and
    /// distinct between kernels (the data dir is used).
    pub async fn new(allowed_images: Vec<String>, instance: String) -> Result<Self, AgentOSError> {
        let client = Docker::connect_with_socket_defaults()
            .map_err(|e| runtime_err(format!("Docker connection failed: {e}")))?;

        // Verify connectivity
        client
            .ping()
            .await
            .map_err(|e| runtime_err(format!("Docker daemon unreachable: {e}")))?;

        remove_leftover_containers(&client, &instance).await;

        Ok(Self {
            client,
            instance,
            containers: Arc::new(RwLock::new(HashMap::new())),
            allowlist: crate::allowlist::ImageAllowlist::new(allowed_images),
        })
    }
}

/// Force-remove containers a previous kernel run left behind. The runtime
/// tracks containers only in memory, so after a restart these would never be
/// reaped, never count toward quota, and could not be destroyed by their owner.
///
/// ponytail: removal, not re-adoption. Rebuilding the map from the
/// `agentos.*` labels (plus `inspect_container` for limits) would let work
/// survive a restart; do that if agents need long-lived containers.
async fn remove_leftover_containers(client: &Docker, instance: &str) {
    let instance_label = format!("{LABEL_PREFIX}.instance={instance}");
    let filters = HashMap::from([("label", vec![instance_label.as_str()])]);
    let opts = ListContainersOptions {
        all: true,
        filters,
        ..Default::default()
    };
    let leftovers = match client.list_containers(Some(opts)).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(error = %e, "Could not list leftover agent containers");
            return;
        }
    };
    for id in leftovers.into_iter().filter_map(|c| c.id) {
        let rm = RemoveContainerOptions {
            force: true,
            v: true,
            ..Default::default()
        };
        match client.remove_container(&id, Some(rm)).await {
            Ok(()) => tracing::info!(container_id = %id, "Removed leftover agent container"),
            Err(e) => {
                tracing::warn!(container_id = %id, error = %e, "Leftover container removal failed")
            }
        }
    }
}

impl DockerRuntime {
    /// Pull an (already allowlisted) image if it is not present locally;
    /// `create_container` does not pull on its own.
    async fn ensure_image(&self, image: &str) -> Result<(), AgentOSError> {
        if self.client.inspect_image(image).await.is_ok() {
            return Ok(());
        }
        let (from_image, tag) = split_image_ref(image);
        tracing::info!(image, "Pulling container image");
        let opts = CreateImageOptions {
            from_image,
            tag,
            ..Default::default()
        };
        let mut pull = self.client.create_image(Some(opts), None, None);
        let drain = async {
            while let Some(step) = pull.next().await {
                step.map_err(|e| runtime_err(format!("Image pull failed for '{image}': {e}")))?;
            }
            Ok(())
        };
        // Bollard's own timeout covers only the response headers, not the
        // streamed pull; a stalled registry would otherwise hang the caller.
        tokio::time::timeout(PULL_TIMEOUT, drain)
            .await
            .map_err(|_| runtime_err(format!("Image pull timed out for '{image}'")))?
    }
}

/// Split `repo[:tag]` for the pull API, which pulls EVERY tag when `tag` is
/// empty. A `:` before the last `/` is a registry port, not a tag. Digest
/// references (`repo@sha256:…`) go through whole with no tag.
fn split_image_ref(image: &str) -> (&str, &str) {
    if image.contains('@') {
        return (image, "");
    }
    let name_start = image.rfind('/').map_or(0, |i| i + 1);
    match image[name_start..].rfind(':') {
        Some(i) => (&image[..name_start + i], &image[name_start + i + 1..]),
        None => (image, "latest"),
    }
}

#[async_trait]
impl ComputeRuntime for DockerRuntime {
    async fn prepare_image(&self, image: &str) -> Result<(), AgentOSError> {
        if !self.allowlist.is_allowed(image) {
            return Err(runtime_err(format!(
                "Image '{image}' is not in the operator allowlist"
            )));
        }
        self.ensure_image(image).await
    }

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

        // No-op when the kernel already called `prepare_image` (outside its
        // provision lock); pulls here for any other caller.
        self.ensure_image(&spec.image).await?;

        let now = Utc::now();
        let expires_at = now + Duration::seconds(spec.ttl_seconds as i64);

        // Build labels for tracking managed containers
        let mut labels = HashMap::new();
        labels.insert(format!("{LABEL_PREFIX}.managed"), "true".to_string());
        labels.insert(format!("{LABEL_PREFIX}.instance"), self.instance.clone());
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
                                    bollard::container::LogOutput::StdOut { .. }
                                        if stdout.len() + text.len() <= MAX_OUTPUT_BYTES =>
                                    {
                                        stdout.push_str(&text);
                                    }
                                    bollard::container::LogOutput::StdErr { .. }
                                        if stderr.len() + text.len() <= MAX_OUTPUT_BYTES =>
                                    {
                                        stderr.push_str(&text);
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
            // Docker reads tail=0 as zero lines; the tool contract says 0 = all.
            tail: if tail == 0 {
                "all".to_string()
            } else {
                tail.to_string()
            },
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
        // Force remove. No graceful stop: PID 1 is `sleep infinity`, which
        // ignores SIGTERM, so a stop always waited out the full grace period.
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

#[cfg(test)]
mod tests {
    use super::split_image_ref;

    #[test]
    fn split_image_ref_never_leaves_tag_empty_for_a_name() {
        assert_eq!(split_image_ref("alpine:3.19"), ("alpine", "3.19"));
        assert_eq!(split_image_ref("n8nio/n8n"), ("n8nio/n8n", "latest"));
        assert_eq!(
            split_image_ref("localhost:5000/team/app:1.2"),
            ("localhost:5000/team/app", "1.2")
        );
        assert_eq!(
            split_image_ref("localhost:5000/team/app"),
            ("localhost:5000/team/app", "latest")
        );
        assert_eq!(split_image_ref("repo@sha256:abc"), ("repo@sha256:abc", ""));
    }

    /// Live round trip against the local Docker daemon, pull included:
    /// `cargo test -p agentos-runtime -- --ignored docker_round_trip`
    #[tokio::test]
    #[ignore = "needs a Docker daemon and network"]
    async fn docker_round_trip() {
        use crate::runtime::{ComputeRuntime, ContainerSpec};
        let image = "alpine:3.19".to_string();
        let instance = format!("test-{}", uuid::Uuid::new_v4());
        let rt = super::DockerRuntime::new(vec![image.clone()], instance)
            .await
            .unwrap();
        let spec = ContainerSpec {
            image,
            memory_limit_bytes: 64 * 1024 * 1024,
            ttl_seconds: 120,
            ..Default::default()
        };
        let info = rt
            .provision(spec, agentos_types::AgentID::new(), None)
            .await
            .unwrap();
        let out = rt
            .exec(&info.id, vec!["echo".into(), "hi".into()], 10_000)
            .await;
        rt.destroy(&info.id).await.unwrap();
        let out = out.unwrap();
        assert_eq!((out.exit_code, out.stdout.trim()), (0, "hi"));
    }
}
