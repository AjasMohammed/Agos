use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
use uuid::Uuid;

use crate::hal::HalDriver;

const DISPLAY_DEVICE_PREFIX: &str = "display:";
const DEFAULT_AUTO_REVERT_SECS: u64 = 15;
const MIN_AUTO_REVERT_SECS: u64 = 5;
const MAX_AUTO_REVERT_SECS: u64 = 300;
const MAX_POSITION: i32 = 32_768;
const MIN_SCALE: f64 = 0.25;
const MAX_SCALE: f64 = 4.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DisplayMode {
    width: u32,
    height: u32,
    refresh_hz: Option<f64>,
    label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DisplayOutput {
    output: String,
    device_id: String,
    connector: String,
    connected: bool,
    enabled: bool,
    current_mode: Option<DisplayMode>,
    available_modes: Vec<DisplayMode>,
    position: Option<DisplayPosition>,
    scale: Option<f64>,
    backend: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DisplayPosition {
    x: i32,
    y: i32,
}

#[derive(Clone, Debug)]
struct DisplayTopology {
    outputs: Vec<DisplayOutput>,
}

#[derive(Clone, Debug, Serialize)]
struct PendingDisplayChange {
    config_id: String,
    change_summary: Value,
    rollback_topology: Value,
    desired_topology: Value,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    backend: String,
}

#[derive(Clone, Debug)]
struct CommandResult {
    status_code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Clone, Debug)]
enum BackendKind {
    XRandr,
    WlrRandr,
    Sysfs,
}

#[async_trait]
trait CommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[String]) -> Result<CommandResult, AgentOSError>;
}

struct SystemCommandRunner;

#[async_trait]
impl CommandRunner for SystemCommandRunner {
    async fn run(&self, program: &str, args: &[String]) -> Result<CommandResult, AgentOSError> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| {
                AgentOSError::HalError(format!("Failed to spawn '{program}': {error}"))
            })?;

        Ok(CommandResult {
            status_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

#[async_trait]
trait DisplayBackend: Send + Sync {
    fn name(&self) -> &'static str;
    async fn list_outputs(&self) -> Result<DisplayTopology, AgentOSError>;
    async fn test_configuration(&self, desired: &DisplayTopology) -> Result<Value, AgentOSError>;
    async fn apply_configuration(&self, desired: &DisplayTopology) -> Result<(), AgentOSError>;
}

/// Display output management with staged apply/confirm/revert semantics.
///
/// The driver is designed for long-lived autonomous workflows:
/// - stable JSON action surface
/// - validation before mutation
/// - auto-revert after a confirmation window
/// - serialized apply/revert to avoid overlapping monitor changes
pub struct DisplayDriver {
    backend: Arc<dyn DisplayBackend>,
    pending: Arc<RwLock<HashMap<String, PendingDisplayChange>>>,
    apply_lock: Arc<Mutex<()>>,
    auto_revert_timeout: Duration,
}

impl Default for DisplayDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayDriver {
    pub fn new() -> Self {
        Self {
            backend: Arc::new(SystemDisplayBackend::new(Arc::new(SystemCommandRunner))),
            pending: Arc::new(RwLock::new(HashMap::new())),
            apply_lock: Arc::new(Mutex::new(())),
            auto_revert_timeout: Duration::from_secs(DEFAULT_AUTO_REVERT_SECS),
        }
    }

    #[cfg(test)]
    fn with_backend(backend: Arc<dyn DisplayBackend>, auto_revert_timeout: Duration) -> Self {
        Self {
            backend,
            pending: Arc::new(RwLock::new(HashMap::new())),
            apply_lock: Arc::new(Mutex::new(())),
            auto_revert_timeout,
        }
    }

    fn action_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'action' param".into()))
    }

    fn output_name_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        let output = params
            .get("output")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'output' param".into()))?;

        if output.is_empty() {
            return Err(AgentOSError::HalError(
                "Invalid 'output' param: cannot be empty".into(),
            ));
        }

        if !output
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        {
            return Err(AgentOSError::HalError(
                "Invalid 'output' param: unsupported characters".into(),
            ));
        }

        Ok(output)
    }

    fn config_id_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        params
            .get("config_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AgentOSError::HalError("Missing 'config_id' param".into()))
    }

    fn find_output_mut<'a>(
        &self,
        topology: &'a mut DisplayTopology,
        output_name: &str,
    ) -> Result<&'a mut DisplayOutput, AgentOSError> {
        topology
            .outputs
            .iter_mut()
            .find(|output| output.output == output_name)
            .ok_or_else(|| {
                AgentOSError::HalError(format!("Unknown display output '{output_name}'"))
            })
    }

    fn auto_revert_timeout_from_params(&self, params: &Value) -> Result<Duration, AgentOSError> {
        let Some(seconds) = params
            .get("auto_revert_timeout_secs")
            .and_then(Value::as_u64)
        else {
            return Ok(self.auto_revert_timeout);
        };

        if !(MIN_AUTO_REVERT_SECS..=MAX_AUTO_REVERT_SECS).contains(&seconds) {
            return Err(AgentOSError::HalError(format!(
                "'auto_revert_timeout_secs' must be between {MIN_AUTO_REVERT_SECS} and {MAX_AUTO_REVERT_SECS}"
            )));
        }

        Ok(Duration::from_secs(seconds))
    }

    fn mode_from_params(&self, params: &Value) -> Result<DisplayMode, AgentOSError> {
        let width = params
            .get("width")
            .and_then(Value::as_u64)
            .ok_or_else(|| AgentOSError::HalError("Missing 'width' param".into()))?;
        let height = params
            .get("height")
            .and_then(Value::as_u64)
            .ok_or_else(|| AgentOSError::HalError("Missing 'height' param".into()))?;

        if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
            return Err(AgentOSError::HalError(
                "Invalid resolution: width/height must be between 1 and 16384".into(),
            ));
        }

        let refresh_hz = params.get("refresh_hz").and_then(Value::as_f64);
        if let Some(refresh_hz) = refresh_hz {
            if !(1.0..=1000.0).contains(&refresh_hz) {
                return Err(AgentOSError::HalError(
                    "Invalid 'refresh_hz' param: must be between 1.0 and 1000.0".into(),
                ));
            }
        }

        Ok(DisplayMode {
            width: width as u32,
            height: height as u32,
            refresh_hz,
            label: mode_label(width as u32, height as u32, refresh_hz),
        })
    }

    fn position_from_params(&self, params: &Value) -> Result<DisplayPosition, AgentOSError> {
        let x = params
            .get("x")
            .and_then(Value::as_i64)
            .ok_or_else(|| AgentOSError::HalError("Missing 'x' param".into()))?;
        let y = params
            .get("y")
            .and_then(Value::as_i64)
            .ok_or_else(|| AgentOSError::HalError("Missing 'y' param".into()))?;

        if x < -MAX_POSITION as i64
            || x > MAX_POSITION as i64
            || y < -MAX_POSITION as i64
            || y > MAX_POSITION as i64
        {
            return Err(AgentOSError::HalError(format!(
                "Display position must be within +/-{MAX_POSITION}"
            )));
        }

        Ok(DisplayPosition {
            x: x as i32,
            y: y as i32,
        })
    }

    fn scale_from_params(&self, params: &Value) -> Result<f64, AgentOSError> {
        let scale = params
            .get("scale")
            .and_then(Value::as_f64)
            .ok_or_else(|| AgentOSError::HalError("Missing 'scale' param".into()))?;
        if !(MIN_SCALE..=MAX_SCALE).contains(&scale) {
            return Err(AgentOSError::HalError(format!(
                "'scale' must be between {MIN_SCALE} and {MAX_SCALE}"
            )));
        }
        Ok(scale)
    }

    fn validate_output_mode(
        &self,
        output: &DisplayOutput,
        mode: &DisplayMode,
    ) -> Result<(), AgentOSError> {
        let mode_exists = output.available_modes.iter().any(|candidate| {
            candidate.width == mode.width
                && candidate.height == mode.height
                && refresh_matches(candidate.refresh_hz, mode.refresh_hz)
        });

        if !mode_exists {
            return Err(AgentOSError::HalError(format!(
                "Requested mode '{}' is not available on output '{}'",
                mode.label, output.output
            )));
        }

        Ok(())
    }

    fn ensure_safe_topology(&self, topology: &DisplayTopology) -> Result<(), AgentOSError> {
        let enabled_connected = topology
            .outputs
            .iter()
            .filter(|output| output.connected && output.enabled)
            .count();
        if enabled_connected == 0 {
            return Err(AgentOSError::HalError(
                "Refusing to apply a display configuration that disables all connected outputs"
                    .into(),
            ));
        }
        Ok(())
    }

    fn desired_topology_for_action(
        &self,
        action: &str,
        params: &Value,
        current: &DisplayTopology,
    ) -> Result<(DisplayTopology, Value), AgentOSError> {
        let mut desired = current.clone();
        let output_name = self.output_name_from_params(params)?;
        let output = self.find_output_mut(&mut desired, output_name)?;

        if !output.connected {
            return Err(AgentOSError::HalError(format!(
                "Display output '{}' is not connected",
                output_name
            )));
        }

        let summary = match action {
            "set_mode" => {
                let mode = self.mode_from_params(params)?;
                self.validate_output_mode(output, &mode)?;
                output.current_mode = Some(mode.clone());
                output.enabled = true;
                json!({
                    "operation": "set_mode",
                    "output": output_name,
                    "mode": mode,
                })
            }
            "set_position" => {
                let position = self.position_from_params(params)?;
                output.position = Some(position.clone());
                json!({
                    "operation": "set_position",
                    "output": output_name,
                    "position": position,
                })
            }
            "set_scale" => {
                let scale = self.scale_from_params(params)?;
                output.scale = Some(scale);
                json!({
                    "operation": "set_scale",
                    "output": output_name,
                    "scale": scale,
                })
            }
            "enable" => {
                output.enabled = true;
                if output.current_mode.is_none() {
                    output.current_mode = output.available_modes.first().cloned();
                }
                json!({
                    "operation": "enable",
                    "output": output_name,
                })
            }
            "disable" => {
                output.enabled = false;
                json!({
                    "operation": "disable",
                    "output": output_name,
                })
            }
            other => {
                return Err(AgentOSError::HalError(format!(
                    "Unsupported display mutation action: {other}"
                )));
            }
        };

        self.ensure_safe_topology(&desired)?;
        Ok((desired, summary))
    }

    async fn list(&self) -> Result<Value, AgentOSError> {
        let topology = self.backend.list_outputs().await?;
        Ok(json!({
            "backend": self.backend.name(),
            "outputs": topology.outputs,
        }))
    }

    async fn test_change(&self, params: &Value) -> Result<Value, AgentOSError> {
        let operation = params
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'operation' param".into()))?;
        let current = self.backend.list_outputs().await?;
        let (desired, summary) = self.desired_topology_for_action(operation, params, &current)?;
        let backend_result = self.backend.test_configuration(&desired).await?;

        Ok(json!({
            "valid": true,
            "backend": self.backend.name(),
            "operation": summary,
            "backend_result": backend_result,
            "current_topology": current.outputs,
            "desired_topology": desired.outputs,
        }))
    }

    async fn apply_change(&self, action: &str, params: &Value) -> Result<Value, AgentOSError> {
        let timeout = self.auto_revert_timeout_from_params(params)?;

        // Acquire apply_lock first to prevent TOCTOU: the rollback snapshot must
        // reflect the state at the moment we hold exclusive apply rights.
        let _guard = self.apply_lock.lock().await;
        let current = self.backend.list_outputs().await?;
        let (desired, summary) = self.desired_topology_for_action(action, params, &current)?;
        let rollback_json = serde_json::to_value(&current.outputs)
            .map_err(|error| AgentOSError::Serialization(error.to_string()))?;
        let desired_json = serde_json::to_value(&desired.outputs)
            .map_err(|error| AgentOSError::Serialization(error.to_string()))?;

        let backend_test = self.backend.test_configuration(&desired).await?;
        self.backend.apply_configuration(&desired).await?;

        let config_id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let expires_at = created_at
            + chrono::Duration::from_std(timeout)
                .map_err(|error| AgentOSError::HalError(error.to_string()))?;

        let pending = PendingDisplayChange {
            config_id: config_id.clone(),
            change_summary: summary.clone(),
            rollback_topology: rollback_json.clone(),
            desired_topology: desired_json.clone(),
            expires_at,
            created_at,
            backend: self.backend.name().to_string(),
        };

        // Release apply_lock before writing to pending to prevent ABBA deadlock:
        // auto-revert tasks acquire pending → apply_lock, so we must not hold
        // apply_lock while acquiring pending.
        drop(_guard);

        self.pending
            .write()
            .await
            .insert(config_id.clone(), pending.clone());

        let pending_store = Arc::clone(&self.pending);
        let backend = Arc::clone(&self.backend);
        let apply_lock = Arc::clone(&self.apply_lock);
        let rollback_topology = current.clone();
        let config_id_for_task = config_id.clone();
        tokio::spawn(async move {
            sleep(timeout).await;
            // Lock ordering: pending first, then apply_lock (consistent everywhere).
            let expired = pending_store.write().await.remove(&config_id_for_task);
            if expired.is_some() {
                let _guard = apply_lock.lock().await;
                if let Err(error) = backend.apply_configuration(&rollback_topology).await {
                    tracing::error!(
                        config_id = %config_id_for_task,
                        error = %error,
                        "Display auto-revert failed"
                    );
                } else {
                    tracing::warn!(
                        config_id = %config_id_for_task,
                        "Display configuration auto-reverted after confirmation timeout"
                    );
                }
            }
        });

        Ok(json!({
            "status": "applied_pending_confirmation",
            "backend": self.backend.name(),
            "config_id": config_id,
            "operation": summary,
            "backend_test": backend_test,
            "auto_revert_timeout_secs": timeout.as_secs(),
            "confirmation_deadline": expires_at,
            "confirmation_required": true,
            "desired_topology": desired.outputs,
        }))
    }

    async fn confirm_change(&self, params: &Value) -> Result<Value, AgentOSError> {
        let config_id = self.config_id_from_params(params)?;
        let removed = self
            .pending
            .write()
            .await
            .remove(config_id)
            .ok_or_else(|| {
                AgentOSError::HalError(format!(
                    "Unknown or expired display config_id '{config_id}'"
                ))
            })?;

        Ok(json!({
            "status": "confirmed",
            "backend": self.backend.name(),
            "config_id": config_id,
            "confirmed_at": Utc::now(),
            "operation": removed.change_summary,
        }))
    }

    async fn revert_change(&self, params: &Value) -> Result<Value, AgentOSError> {
        let config_id = self.config_id_from_params(params)?;
        let pending = self
            .pending
            .write()
            .await
            .remove(config_id)
            .ok_or_else(|| {
                AgentOSError::HalError(format!(
                    "Unknown or expired display config_id '{config_id}'"
                ))
            })?;

        let rollback = self.backend.list_outputs().await?;
        let _guard = self.apply_lock.lock().await;
        let rollback_topology = topology_from_value(&pending.rollback_topology)?;
        self.backend.apply_configuration(&rollback_topology).await?;

        Ok(json!({
            "status": "reverted",
            "backend": self.backend.name(),
            "config_id": config_id,
            "reverted_at": Utc::now(),
            "operation": pending.change_summary,
            "previous_topology": rollback.outputs,
            "restored_topology": rollback_topology.outputs,
        }))
    }
}

#[async_trait]
impl HalDriver for DisplayDriver {
    fn name(&self) -> &str {
        "display"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.display", PermissionOp::Read)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => ("hardware.display", PermissionOp::Read),
            "test" => ("hardware.display", PermissionOp::Query),
            "confirm" | "revert" | "set_mode" | "set_position" | "set_scale" | "enable"
            | "disable" => ("hardware.display.config", PermissionOp::Write),
            _ => self.required_permission(),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "set_mode" | "set_position" | "set_scale" | "enable" | "disable" | "test" => params
                .get("output")
                .and_then(Value::as_str)
                .map(|output| format!("{DISPLAY_DEVICE_PREFIX}{output}")),
            _ => None,
        }
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match self.action_from_params(&params)? {
            "list" => self.list().await,
            "test" => self.test_change(&params).await,
            action @ ("set_mode" | "set_position" | "set_scale" | "enable" | "disable") => {
                self.apply_change(action, &params).await
            }
            "confirm" => self.confirm_change(&params).await,
            "revert" => self.revert_change(&params).await,
            other => Err(AgentOSError::HalError(format!(
                "Unknown display action: {other}"
            ))),
        }
    }
}

struct SystemDisplayBackend {
    runner: Arc<dyn CommandRunner>,
    kind: BackendKind,
}

impl SystemDisplayBackend {
    fn new(runner: Arc<dyn CommandRunner>) -> Self {
        let kind = detect_backend_kind();
        Self { runner, kind }
    }

    async fn run_checked(
        &self,
        program: &str,
        args: &[String],
    ) -> Result<CommandResult, AgentOSError> {
        let result = self.runner.run(program, args).await?;
        if result.status_code != 0 {
            return Err(AgentOSError::HalError(format!(
                "{program} failed with exit code {}: {}",
                result.status_code,
                trimmed_command_error(&result.stderr, &result.stdout)
            )));
        }
        Ok(result)
    }

    async fn list_from_sysfs(
        &self,
        backend_name: &'static str,
    ) -> Result<DisplayTopology, AgentOSError> {
        let name = backend_name.to_string();
        tokio::task::spawn_blocking(move || list_from_sysfs_blocking(&name))
            .await
            .map_err(|error| AgentOSError::HalError(format!("sysfs list task panicked: {error}")))?
    }

    async fn xrandr_test(&self, desired: &DisplayTopology) -> Result<Value, AgentOSError> {
        let args = xrandr_args_for_topology(desired, true)?;
        let result = self.run_checked("xrandr", &args).await?;
        Ok(json!({
            "backend_tested": true,
            "command": "xrandr",
            "args": args,
            "stdout": trim_output(&result.stdout),
        }))
    }

    async fn xrandr_apply(&self, desired: &DisplayTopology) -> Result<(), AgentOSError> {
        let args = xrandr_args_for_topology(desired, false)?;
        self.run_checked("xrandr", &args).await?;
        Ok(())
    }

    async fn wlr_test(&self, desired: &DisplayTopology) -> Result<Value, AgentOSError> {
        let args = wlr_randr_args_for_topology(desired, true)?;
        let result = self.run_checked("wlr-randr", &args).await?;
        Ok(json!({
            "backend_tested": true,
            "command": "wlr-randr",
            "args": args,
            "stdout": trim_output(&result.stdout),
        }))
    }

    async fn wlr_apply(&self, desired: &DisplayTopology) -> Result<(), AgentOSError> {
        let args = wlr_randr_args_for_topology(desired, false)?;
        self.run_checked("wlr-randr", &args).await?;
        Ok(())
    }
}

#[async_trait]
impl DisplayBackend for SystemDisplayBackend {
    fn name(&self) -> &'static str {
        match self.kind {
            BackendKind::XRandr => "xrandr",
            BackendKind::WlrRandr => "wlr-randr",
            BackendKind::Sysfs => "sysfs",
        }
    }

    async fn list_outputs(&self) -> Result<DisplayTopology, AgentOSError> {
        self.list_from_sysfs(self.name()).await
    }

    async fn test_configuration(&self, desired: &DisplayTopology) -> Result<Value, AgentOSError> {
        match self.kind {
            BackendKind::XRandr => self.xrandr_test(desired).await,
            BackendKind::WlrRandr => self.wlr_test(desired).await,
            BackendKind::Sysfs => Ok(json!({
                "backend_tested": false,
                "backend": "sysfs",
                "reason": "No runtime display management backend available; local validation succeeded",
            })),
        }
    }

    async fn apply_configuration(&self, desired: &DisplayTopology) -> Result<(), AgentOSError> {
        match self.kind {
            BackendKind::XRandr => self.xrandr_apply(desired).await,
            BackendKind::WlrRandr => self.wlr_apply(desired).await,
            BackendKind::Sysfs => Err(AgentOSError::HalError(
                "No runtime display management backend available; install xrandr or wlr-randr in a session with DISPLAY/WAYLAND_DISPLAY"
                    .into(),
            )),
        }
    }
}

fn detect_backend_kind() -> BackendKind {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && command_exists("wlr-randr") {
        return BackendKind::WlrRandr;
    }
    if std::env::var_os("DISPLAY").is_some() && command_exists("xrandr") {
        return BackendKind::XRandr;
    }
    BackendKind::Sysfs
}

fn list_from_sysfs_blocking(backend_name: &str) -> Result<DisplayTopology, AgentOSError> {
    let root = PathBuf::from("/sys/class/drm");
    let entries = std::fs::read_dir(&root).map_err(|error| {
        AgentOSError::HalError(format!(
            "Unable to read DRM connector state from '{}': {error}",
            root.display()
        ))
    })?;

    let mut outputs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || !name.contains('-') {
            continue;
        }

        let connector_path = entry.path();
        let connected = read_trimmed(connector_path.join("status"))
            .map(|status| status == "connected")
            .unwrap_or(false);
        let enabled = read_trimmed(connector_path.join("enabled"))
            .map(|value| value == "enabled")
            .unwrap_or(false);
        let available_modes = parse_sysfs_modes(&connector_path.join("modes"));
        let current_mode = if enabled {
            available_modes.first().cloned()
        } else {
            None
        };

        outputs.push(DisplayOutput {
            output: connector_name(&name),
            device_id: format!("{DISPLAY_DEVICE_PREFIX}{}", connector_name(&name)),
            connector: name,
            connected,
            enabled,
            current_mode,
            available_modes,
            position: None,
            scale: Some(1.0),
            backend: backend_name.to_string(),
        });
    }

    outputs.sort_by(|left, right| left.output.cmp(&right.output));
    Ok(DisplayTopology { outputs })
}

fn parse_sysfs_modes(path: &Path) -> Vec<DisplayMode> {
    std::fs::read_to_string(path)
        .ok()
        .map(|content| {
            content
                .lines()
                .filter_map(parse_mode_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

fn connector_name(connector: &str) -> String {
    connector
        .split_once('-')
        .map(|(_, output)| output.to_string())
        .unwrap_or_else(|| connector.to_string())
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|path| path.join(program))
                .find(|candidate| candidate.is_file())
        })
        .is_some()
}

fn parse_mode_string(mode: &str) -> Option<DisplayMode> {
    let mode = mode.trim();
    if mode.is_empty() {
        return None;
    }

    let (resolution, refresh_hz) = if let Some((resolution, refresh)) = mode.split_once('@') {
        (
            resolution,
            refresh.trim_end_matches("Hz").parse::<f64>().ok(),
        )
    } else {
        (mode, None)
    };
    let (width, height) = resolution.split_once('x')?;
    let width = width.parse::<u32>().ok()?;
    let height = height.parse::<u32>().ok()?;
    Some(DisplayMode {
        width,
        height,
        refresh_hz,
        label: mode_label(width, height, refresh_hz),
    })
}

fn mode_label(width: u32, height: u32, refresh_hz: Option<f64>) -> String {
    match refresh_hz {
        Some(refresh_hz) => format!("{width}x{height}@{refresh_hz:.2}Hz"),
        None => format!("{width}x{height}"),
    }
}

fn refresh_matches(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => (left - right).abs() < 0.01,
        (_, None) | (None, _) => true,
    }
}

fn topology_from_value(value: &Value) -> Result<DisplayTopology, AgentOSError> {
    let outputs: Vec<DisplayOutput> = serde_json::from_value(value.clone())
        .map_err(|error| AgentOSError::Serialization(error.to_string()))?;
    Ok(DisplayTopology { outputs })
}

fn xrandr_args_for_topology(
    topology: &DisplayTopology,
    dry_run: bool,
) -> Result<Vec<String>, AgentOSError> {
    let mut args = Vec::new();
    if dry_run {
        args.push("--dryrun".to_string());
    }

    for output in &topology.outputs {
        args.push("--output".to_string());
        args.push(output.output.clone());

        if !output.connected || !output.enabled {
            args.push("--off".to_string());
            continue;
        }

        let mode = output.current_mode.as_ref().ok_or_else(|| {
            AgentOSError::HalError(format!(
                "Display output '{}' is enabled without a current mode",
                output.output
            ))
        })?;

        args.push("--mode".to_string());
        args.push(format!("{}x{}", mode.width, mode.height));
        if let Some(refresh_hz) = mode.refresh_hz {
            args.push("--rate".to_string());
            args.push(format!("{refresh_hz:.2}"));
        }
        if let Some(position) = &output.position {
            args.push("--pos".to_string());
            args.push(format!("{}x{}", position.x, position.y));
        }
        if let Some(scale) = output.scale {
            args.push("--scale".to_string());
            args.push(format!("{scale:.3}x{scale:.3}"));
        }
    }

    Ok(args)
}

fn wlr_randr_args_for_topology(
    topology: &DisplayTopology,
    dry_run: bool,
) -> Result<Vec<String>, AgentOSError> {
    let mut args = Vec::new();
    if dry_run {
        args.push("--dryrun".to_string());
    }

    for output in &topology.outputs {
        args.push("--output".to_string());
        args.push(output.output.clone());

        if !output.connected || !output.enabled {
            args.push("--off".to_string());
            continue;
        }

        let mode = output.current_mode.as_ref().ok_or_else(|| {
            AgentOSError::HalError(format!(
                "Display output '{}' is enabled without a current mode",
                output.output
            ))
        })?;

        args.push("--mode".to_string());
        let mode_arg = match mode.refresh_hz {
            Some(refresh_hz) => format!("{}x{}@{refresh_hz:.2}Hz", mode.width, mode.height),
            None => format!("{}x{}", mode.width, mode.height),
        };
        args.push(mode_arg);
        if let Some(position) = &output.position {
            args.push("--pos".to_string());
            args.push(format!("{},{}", position.x, position.y));
        }
        if let Some(scale) = output.scale {
            args.push("--scale".to_string());
            args.push(format!("{scale:.3}"));
        }
    }

    Ok(args)
}

fn trim_output(value: &str) -> String {
    value.trim().to_string()
}

fn trimmed_command_error(stderr: &str, stdout: &str) -> String {
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    stdout.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct MockBackend {
        current: RwLock<DisplayTopology>,
        applied: Mutex<Vec<DisplayTopology>>,
        test_calls: Mutex<u32>,
    }

    impl MockBackend {
        fn new(topology: DisplayTopology) -> Self {
            Self {
                current: RwLock::new(topology),
                applied: Mutex::new(Vec::new()),
                test_calls: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl DisplayBackend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn list_outputs(&self) -> Result<DisplayTopology, AgentOSError> {
            Ok(self.current.read().await.clone())
        }

        async fn test_configuration(
            &self,
            _desired: &DisplayTopology,
        ) -> Result<Value, AgentOSError> {
            *self.test_calls.lock().await += 1;
            Ok(json!({ "backend_tested": true }))
        }

        async fn apply_configuration(&self, desired: &DisplayTopology) -> Result<(), AgentOSError> {
            self.applied.lock().await.push(desired.clone());
            *self.current.write().await = desired.clone();
            Ok(())
        }
    }

    fn test_topology() -> DisplayTopology {
        DisplayTopology {
            outputs: vec![
                DisplayOutput {
                    output: "eDP-1".to_string(),
                    device_id: "display:card1-eDP-1".to_string(),
                    connector: "card1-eDP-1".to_string(),
                    connected: true,
                    enabled: true,
                    current_mode: Some(DisplayMode {
                        width: 1920,
                        height: 1200,
                        refresh_hz: Some(60.0),
                        label: "1920x1200@60.00Hz".to_string(),
                    }),
                    available_modes: vec![
                        DisplayMode {
                            width: 1920,
                            height: 1200,
                            refresh_hz: Some(60.0),
                            label: "1920x1200@60.00Hz".to_string(),
                        },
                        DisplayMode {
                            width: 1280,
                            height: 720,
                            refresh_hz: Some(60.0),
                            label: "1280x720@60.00Hz".to_string(),
                        },
                    ],
                    position: Some(DisplayPosition { x: 0, y: 0 }),
                    scale: Some(1.0),
                    backend: "mock".to_string(),
                },
                DisplayOutput {
                    output: "HDMI-A-1".to_string(),
                    device_id: "display:card1-HDMI-A-1".to_string(),
                    connector: "card1-HDMI-A-1".to_string(),
                    connected: true,
                    enabled: false,
                    current_mode: None,
                    available_modes: vec![DisplayMode {
                        width: 1920,
                        height: 1080,
                        refresh_hz: Some(60.0),
                        label: "1920x1080@60.00Hz".to_string(),
                    }],
                    position: Some(DisplayPosition { x: 1920, y: 0 }),
                    scale: Some(1.0),
                    backend: "mock".to_string(),
                },
            ],
        }
    }

    #[tokio::test]
    async fn auto_revert_rolls_back_unconfirmed_change() {
        let backend = Arc::new(MockBackend::new(test_topology()));
        let driver = DisplayDriver::with_backend(backend.clone(), Duration::from_millis(25));
        let response = driver
            .query(json!({
                "action": "set_mode",
                "output": "eDP-1",
                "width": 1280,
                "height": 720,
                "refresh_hz": 60.0,
            }))
            .await
            .unwrap();

        assert_eq!(response["status"], "applied_pending_confirmation");
        tokio::time::sleep(Duration::from_millis(60)).await;

        let applied = backend.applied.lock().await;
        assert_eq!(applied.len(), 2);
        assert_eq!(
            applied.last().unwrap().outputs[0]
                .current_mode
                .as_ref()
                .unwrap()
                .width,
            1920
        );
    }

    #[tokio::test]
    async fn invalid_resolution_is_rejected_during_test() {
        let backend = Arc::new(MockBackend::new(test_topology()));
        let driver = DisplayDriver::with_backend(backend, Duration::from_secs(15));

        let error = driver
            .query(json!({
                "action": "test",
                "operation": "set_mode",
                "output": "eDP-1",
                "width": 99999,
                "height": 99999
            }))
            .await
            .unwrap_err();

        assert!(
            matches!(error, AgentOSError::HalError(message) if message.contains("Invalid resolution"))
        );
    }

    #[tokio::test]
    async fn confirm_removes_pending_configuration() {
        let backend = Arc::new(MockBackend::new(test_topology()));
        let driver = DisplayDriver::with_backend(backend, Duration::from_secs(1));

        let response = driver
            .query(json!({
                "action": "enable",
                "output": "HDMI-A-1",
            }))
            .await
            .unwrap();
        let config_id = response["config_id"].as_str().unwrap().to_string();

        let confirmed = driver
            .query(json!({
                "action": "confirm",
                "config_id": config_id,
            }))
            .await
            .unwrap();

        assert_eq!(confirmed["status"], "confirmed");
        assert!(driver.pending.read().await.is_empty());
    }
}
