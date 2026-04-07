use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock};

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::hal::HalDriver;

const DEFAULT_CAPTURE_SECONDS: u64 = 5;
const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u32 = 2;
const MAX_CAPTURE_SECONDS: u64 = 300;
const MAX_SAMPLE_RATE: u32 = 192_000;
const MAX_CHANNELS: u32 = 8;
const MAX_PLAYBACK_BYTES: u64 = 100 * 1024 * 1024;
const DEFAULT_CONSENT_TTL_SECONDS: u64 = 300;
const MAX_CONSENT_TTL_SECONDS: u64 = 3600;
const AUDIO_DEVICE_PREFIX: &str = "audio:";

#[derive(Clone, Debug)]
struct AudioConsentGrant {
    target: String,
    expires_at: DateTime<Utc>,
    granted_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AudioNode {
    object_id: String,
    node_id: String,
    name: String,
    description: String,
    media_class: String,
}

impl AudioNode {
    fn device_id(&self) -> String {
        format!("{AUDIO_DEVICE_PREFIX}{}", self.node_id)
    }

    fn role(&self) -> &'static str {
        if self.media_class.contains("Source") {
            "source"
        } else {
            "sink"
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "device_id": self.device_id(),
            "object_id": self.object_id,
            "node_id": self.node_id,
            "name": self.name,
            "description": self.description,
            "media_class": self.media_class,
            "role": self.role(),
        })
    }
}

#[derive(Clone, Debug)]
struct CommandResult {
    status_code: i32,
    stdout: String,
    stderr: String,
}

#[async_trait]
trait AudioCommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[String]) -> Result<CommandResult, AgentOSError>;
}

struct SystemAudioCommandRunner;

#[async_trait]
impl AudioCommandRunner for SystemAudioCommandRunner {
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

/// PipeWire-backed audio driver using the stable user-space CLI tools.
///
/// The driver is feature-gated and designed for agentic workflows:
/// - action-scoped permissions
/// - TTL-based microphone consent
/// - device-scoped quarantine via `device_key()`
/// - predictable JSON outputs for orchestration
pub struct AudioDriver {
    consent_store: Arc<RwLock<HashMap<String, AudioConsentGrant>>>,
    runner: Arc<dyn AudioCommandRunner>,
}

impl Default for AudioDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDriver {
    pub fn new() -> Self {
        Self {
            consent_store: Arc::new(RwLock::new(HashMap::new())),
            runner: Arc::new(SystemAudioCommandRunner),
        }
    }

    #[cfg(test)]
    fn with_runner(runner: Arc<dyn AudioCommandRunner>) -> Self {
        Self {
            consent_store: Arc::new(RwLock::new(HashMap::new())),
            runner,
        }
    }

    fn action_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'action' param".into()))
    }

    fn sanitize_audio_target<'a>(
        &self,
        params: &'a Value,
        keys: &[&str],
        field_name: &str,
    ) -> Result<Option<&'a str>, AgentOSError> {
        let value = keys
            .iter()
            .find_map(|key| params.get(*key).and_then(Value::as_str));

        let Some(value) = value else {
            return Ok(None);
        };

        if value.is_empty() {
            return Err(AgentOSError::HalError(format!(
                "Invalid '{field_name}' param: cannot be empty"
            )));
        }

        if value.starts_with('-') {
            return Err(AgentOSError::HalError(format!(
                "Invalid '{field_name}' param: must not start with '-'"
            )));
        }

        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-' | '/'))
        {
            return Err(AgentOSError::HalError(format!(
                "Invalid '{field_name}' param: unsupported characters"
            )));
        }

        Ok(Some(value))
    }

    fn normalize_device_key(target: &str) -> String {
        target
            .strip_prefix(AUDIO_DEVICE_PREFIX)
            .unwrap_or(target)
            .to_string()
    }

    fn output_path_from_params(&self, params: &Value) -> Result<PathBuf, AgentOSError> {
        if let Some(path) = params.get("output_path").and_then(Value::as_str) {
            let path = Path::new(path);
            if path.as_os_str().is_empty() {
                return Err(AgentOSError::HalError(
                    "Invalid 'output_path' param: cannot be empty".into(),
                ));
            }
            if !path.is_absolute() {
                return Err(AgentOSError::HalError(
                    "Invalid 'output_path' param: must be an absolute path".into(),
                ));
            }
            if path
                .components()
                .any(|component| component == Component::ParentDir)
            {
                return Err(AgentOSError::HalError(
                    "Invalid 'output_path' param: path traversal rejected".into(),
                ));
            }
            return Ok(path.to_path_buf());
        }

        Ok(std::env::temp_dir().join(format!("agentos-audio-{}.wav", Uuid::new_v4())))
    }

    async fn playback_path_from_params(&self, params: &Value) -> Result<PathBuf, AgentOSError> {
        let raw = params
            .get("audio_path")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'audio_path' param".into()))?;

        let path = Path::new(raw);
        if path.as_os_str().is_empty() {
            return Err(AgentOSError::HalError("Missing 'audio_path' param".into()));
        }
        if !path.is_absolute() {
            return Err(AgentOSError::HalError(
                "Invalid 'audio_path' param: must be an absolute path".into(),
            ));
        }
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(AgentOSError::HalError("Path traversal blocked".into()));
        }

        let metadata = tokio::fs::symlink_metadata(path).await.map_err(|error| {
            AgentOSError::HalError(format!(
                "Unable to read audio file metadata '{}': {error}",
                raw
            ))
        })?;
        if metadata.is_symlink() {
            return Err(AgentOSError::HalError(format!(
                "Audio path '{}' is a symlink — rejected for safety",
                raw
            )));
        }
        if !metadata.is_file() {
            return Err(AgentOSError::HalError(format!(
                "Audio path '{}' is not a regular file",
                raw
            )));
        }
        if metadata.len() > MAX_PLAYBACK_BYTES {
            return Err(AgentOSError::HalError(format!(
                "Audio file exceeds the {} byte playback limit",
                MAX_PLAYBACK_BYTES
            )));
        }

        Ok(path.to_path_buf())
    }

    fn capture_duration_from_params(&self, params: &Value) -> Result<u64, AgentOSError> {
        let duration = params
            .get("duration_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_CAPTURE_SECONDS);
        if duration == 0 || duration > MAX_CAPTURE_SECONDS {
            return Err(AgentOSError::HalError(format!(
                "'duration_seconds' must be between 1 and {MAX_CAPTURE_SECONDS}"
            )));
        }
        Ok(duration)
    }

    fn sample_rate_from_params(&self, params: &Value) -> Result<u32, AgentOSError> {
        let raw = params
            .get("sample_rate")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_SAMPLE_RATE as u64);
        if !(8_000..=MAX_SAMPLE_RATE as u64).contains(&raw) {
            return Err(AgentOSError::HalError(format!(
                "'sample_rate' must be between 8000 and {MAX_SAMPLE_RATE}"
            )));
        }
        Ok(raw as u32)
    }

    fn channels_from_params(&self, params: &Value) -> Result<u32, AgentOSError> {
        let raw = params
            .get("channels")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_CHANNELS as u64);
        if raw == 0 || raw > MAX_CHANNELS as u64 {
            return Err(AgentOSError::HalError(format!(
                "'channels' must be between 1 and {MAX_CHANNELS}"
            )));
        }
        Ok(raw as u32)
    }

    fn volume_from_params(&self, params: &Value) -> Result<f64, AgentOSError> {
        let volume = params
            .get("volume")
            .and_then(Value::as_f64)
            .ok_or_else(|| AgentOSError::HalError("Missing 'volume' param".into()))?;
        if !(0.0..=1.5).contains(&volume) {
            return Err(AgentOSError::HalError(
                "'volume' must be between 0.0 and 1.5".into(),
            ));
        }
        Ok(volume)
    }

    async fn run_checked(
        &self,
        program: &str,
        args: &[String],
        error_context: &str,
    ) -> Result<CommandResult, AgentOSError> {
        let result = self.runner.run(program, args).await?;
        if result.status_code != 0 {
            let stderr = result.stderr.trim();
            let stdout = result.stdout.trim();
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "command failed without diagnostic output"
            };
            return Err(AgentOSError::HalError(format!("{error_context}: {detail}")));
        }
        Ok(result)
    }

    fn parse_pw_cli_nodes(&self, stdout: &str) -> Result<Vec<AudioNode>, AgentOSError> {
        static LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"^\s*([^=]+?)\s*=\s*"?(.*?)"?\s*$"#).expect("valid regex")
        });
        static ID_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^id\s+(\d+),").expect("valid regex"));
        let line_re = &*LINE_RE;
        let id_re = &*ID_RE;

        let mut nodes = Vec::new();
        let mut current_id: Option<String> = None;
        let mut fields = HashMap::new();

        let push_current = |nodes: &mut Vec<AudioNode>,
                            current_id: &Option<String>,
                            fields: &HashMap<String, String>| {
            let Some(object_id) = current_id.clone() else {
                return;
            };
            let Some(media_class) = fields.get("media.class").cloned() else {
                return;
            };
            if !matches!(media_class.as_str(), "Audio/Source" | "Audio/Sink") {
                return;
            }

            let node_id = fields
                .get("object.serial")
                .cloned()
                .unwrap_or_else(|| object_id.clone());
            let name = fields
                .get("node.name")
                .or_else(|| fields.get("node.nick"))
                .cloned()
                .unwrap_or_else(|| format!("node-{node_id}"));
            let description = fields
                .get("node.description")
                .or_else(|| fields.get("node.nick"))
                .or_else(|| fields.get("media.name"))
                .cloned()
                .unwrap_or_else(|| name.clone());

            nodes.push(AudioNode {
                object_id,
                node_id,
                name,
                description,
                media_class,
            });
        };

        for line in stdout.lines() {
            if let Some(captures) = id_re.captures(line) {
                push_current(&mut nodes, &current_id, &fields);
                current_id = captures.get(1).map(|capture| capture.as_str().to_string());
                fields.clear();
                continue;
            }

            let Some(captures) = line_re.captures(line) else {
                continue;
            };
            let key = captures
                .get(1)
                .map(|capture| capture.as_str().trim().to_string())
                .unwrap_or_default();
            let value = captures
                .get(2)
                .map(|capture| capture.as_str().trim().trim_matches('"').to_string())
                .unwrap_or_default();
            if !key.is_empty() {
                fields.insert(key, value);
            }
        }

        push_current(&mut nodes, &current_id, &fields);
        Ok(nodes)
    }

    async fn list_devices(&self) -> Result<Value, AgentOSError> {
        let args = vec!["ls".to_string(), "Node".to_string()];
        let output = self
            .run_checked("pw-cli", &args, "PipeWire device enumeration failed")
            .await?;
        let nodes = self.parse_pw_cli_nodes(&output.stdout)?;
        let sources: Vec<Value> = nodes
            .iter()
            .filter(|node| node.media_class == "Audio/Source")
            .map(AudioNode::to_json)
            .collect();
        let sinks: Vec<Value> = nodes
            .iter()
            .filter(|node| node.media_class == "Audio/Sink")
            .map(AudioNode::to_json)
            .collect();

        Ok(json!({
            "sources": sources,
            "sinks": sinks,
            "source_count": nodes.iter().filter(|node| node.media_class == "Audio/Source").count(),
            "sink_count": nodes.iter().filter(|node| node.media_class == "Audio/Sink").count(),
        }))
    }

    async fn grant_capture_consent(&self, params: &Value) -> Result<Value, AgentOSError> {
        let source = self
            .sanitize_audio_target(params, &["source", "node_id"], "source")?
            .ok_or_else(|| AgentOSError::HalError("Missing 'source' param".into()))?;
        let ttl_seconds = params
            .get("ttl_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_CONSENT_TTL_SECONDS);
        if ttl_seconds == 0 || ttl_seconds > MAX_CONSENT_TTL_SECONDS {
            return Err(AgentOSError::HalError(format!(
                "'ttl_seconds' must be between 1 and {MAX_CONSENT_TTL_SECONDS}"
            )));
        }

        let now = Utc::now();
        let grant = AudioConsentGrant {
            target: Self::normalize_device_key(source),
            granted_at: now,
            expires_at: now + chrono::Duration::seconds(ttl_seconds as i64),
        };

        self.consent_store
            .write()
            .await
            .insert(grant.target.clone(), grant.clone());

        Ok(json!({
            "consent_granted": true,
            "source": grant.target,
            "granted_at": grant.granted_at,
            "expires_at": grant.expires_at,
        }))
    }

    async fn revoke_capture_consent(&self, params: &Value) -> Result<Value, AgentOSError> {
        let source = self
            .sanitize_audio_target(params, &["source", "node_id"], "source")?
            .ok_or_else(|| AgentOSError::HalError("Missing 'source' param".into()))?;
        let removed = self
            .consent_store
            .write()
            .await
            .remove(&Self::normalize_device_key(source))
            .is_some();

        Ok(json!({
            "consent_revoked": removed,
            "source": Self::normalize_device_key(source),
        }))
    }

    async fn list_capture_consents(&self) -> Result<Value, AgentOSError> {
        self.prune_expired_consents().await;
        let grants = self.consent_store.read().await;
        let entries: Vec<Value> = grants
            .values()
            .map(|grant| {
                json!({
                    "source": grant.target,
                    "granted_at": grant.granted_at,
                    "expires_at": grant.expires_at,
                })
            })
            .collect();

        Ok(json!({ "consents": entries }))
    }

    async fn prune_expired_consents(&self) {
        let now = Utc::now();
        self.consent_store
            .write()
            .await
            .retain(|_, grant| grant.expires_at > now);
    }

    async fn ensure_capture_consent(&self, source: &str) -> Result<(), AgentOSError> {
        let source = Self::normalize_device_key(source);
        let now = Utc::now();
        let mut grants = self.consent_store.write().await;
        grants.retain(|_, grant| grant.expires_at > now);
        if grants.contains_key(&source) {
            return Ok(());
        }

        Err(AgentOSError::PermissionDenied {
            resource: "hardware.audio.capture.consent".to_string(),
            operation: "consent_required".to_string(),
        })
    }

    async fn capture_audio(&self, params: &Value) -> Result<Value, AgentOSError> {
        let duration_seconds = self.capture_duration_from_params(params)?;
        let sample_rate = self.sample_rate_from_params(params)?;
        let channels = self.channels_from_params(params)?;
        let output_path = self.output_path_from_params(params)?;
        let source = self
            .sanitize_audio_target(params, &["source", "node_id"], "source")?
            .ok_or_else(|| AgentOSError::HalError("Missing 'source' param".into()))?;

        self.ensure_capture_consent(source).await?;

        let mut args = vec![
            "--signal=INT".to_string(),
            format!("{duration_seconds}s"),
            "pw-record".to_string(),
            "--rate".to_string(),
            sample_rate.to_string(),
            "--channels".to_string(),
            channels.to_string(),
            "--format".to_string(),
            "s16".to_string(),
            "--media-type".to_string(),
            "Audio".to_string(),
            "--media-category".to_string(),
            "Capture".to_string(),
            "--media-role".to_string(),
            "Communication".to_string(),
            "--target".to_string(),
            Self::normalize_device_key(source),
        ];
        if let Some(remote) = self.sanitize_audio_target(params, &["remote"], "remote")? {
            args.push("--remote".to_string());
            args.push(remote.to_string());
        }
        // Positional output path must come last, after all flags
        args.push(output_path.display().to_string());

        self.run_checked("timeout", &args, "PipeWire audio capture failed")
            .await?;

        let metadata = tokio::fs::symlink_metadata(&output_path)
            .await
            .map_err(|error| {
                AgentOSError::HalError(format!(
                    "Audio capture completed but output file '{}' was unreadable: {error}",
                    output_path.display()
                ))
            })?;
        if metadata.len() == 0 {
            return Err(AgentOSError::HalError(
                "Audio capture produced an empty output file".into(),
            ));
        }

        Ok(json!({
            "captured": true,
            "audio_path": output_path.display().to_string(),
            "duration_seconds": duration_seconds,
            "sample_rate": sample_rate,
            "channels": channels,
            "format": "wav",
            "source": Self::normalize_device_key(source),
        }))
    }

    async fn playback_audio(&self, params: &Value) -> Result<Value, AgentOSError> {
        let audio_path = self.playback_path_from_params(params).await?;
        let sink = self.sanitize_audio_target(params, &["sink", "node_id"], "sink")?;
        let mut args = vec![
            "--media-type".to_string(),
            "Audio".to_string(),
            "--media-category".to_string(),
            "Playback".to_string(),
            "--media-role".to_string(),
            "Notification".to_string(),
        ];
        if let Some(sink) = sink {
            args.push("--target".to_string());
            args.push(Self::normalize_device_key(sink));
        }
        args.push(audio_path.display().to_string());

        self.run_checked("pw-play", &args, "PipeWire audio playback failed")
            .await?;

        Ok(json!({
            "played": true,
            "audio_path": audio_path.display().to_string(),
            "sink": sink.map(Self::normalize_device_key),
        }))
    }

    async fn get_volume(&self, params: &Value) -> Result<Value, AgentOSError> {
        let node_id = self
            .sanitize_audio_target(params, &["node_id", "sink", "source"], "node_id")?
            .ok_or_else(|| AgentOSError::HalError("Missing 'node_id' param".into()))?;
        let args = vec![
            "get-volume".to_string(),
            Self::normalize_device_key(node_id),
        ];
        let output = self
            .run_checked("wpctl", &args, "PipeWire volume query failed")
            .await?;
        let mut values = VecDeque::from_iter(output.stdout.split_whitespace());
        let mut volume = None;
        while let Some(token) = values.pop_front() {
            if token.eq_ignore_ascii_case("Volume:") {
                volume = values
                    .pop_front()
                    .and_then(|candidate| candidate.parse::<f64>().ok());
                break;
            }
        }
        let volume = volume.ok_or_else(|| {
            AgentOSError::HalError("Unable to parse PipeWire volume query output".into())
        })?;

        Ok(json!({
            "node_id": Self::normalize_device_key(node_id),
            "volume": volume,
            "muted": output.stdout.contains("MUTED"),
        }))
    }

    async fn set_volume(&self, params: &Value) -> Result<Value, AgentOSError> {
        let node_id = self
            .sanitize_audio_target(params, &["node_id", "sink", "source"], "node_id")?
            .ok_or_else(|| AgentOSError::HalError("Missing 'node_id' param".into()))?;
        let volume = self.volume_from_params(params)?;
        let args = vec![
            "set-volume".to_string(),
            Self::normalize_device_key(node_id),
            format!("{volume:.2}"),
        ];
        self.run_checked("wpctl", &args, "PipeWire volume update failed")
            .await?;

        Ok(json!({
            "updated": true,
            "node_id": Self::normalize_device_key(node_id),
            "volume": volume,
        }))
    }

    async fn volume(&self, params: &Value) -> Result<Value, AgentOSError> {
        if params.get("volume").is_some() {
            self.set_volume(params).await
        } else {
            self.get_volume(params).await
        }
    }
}

#[async_trait]
impl HalDriver for AudioDriver {
    fn name(&self) -> &str {
        "audio"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.audio.list", PermissionOp::Read)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => ("hardware.audio.list", PermissionOp::Read),
            "capture" => ("hardware.audio.capture", PermissionOp::Execute),
            "playback" => ("hardware.audio.playback", PermissionOp::Execute),
            "volume" => {
                if params.get("volume").is_some() {
                    ("hardware.audio.volume", PermissionOp::Write)
                } else {
                    ("hardware.audio.volume", PermissionOp::Read)
                }
            }
            "grant_capture_consent" | "revoke_capture_consent" => {
                ("hardware.audio.capture", PermissionOp::Execute)
            }
            "list_capture_consents" => ("hardware.audio.capture", PermissionOp::Read),
            _ => ("hardware.audio.list", PermissionOp::Read),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list");
        let keys: &[&str] = match action {
            "capture" | "grant_capture_consent" | "revoke_capture_consent" => {
                &["source", "node_id"]
            }
            "playback" => &["sink", "node_id"],
            "volume" => &["node_id", "sink", "source"],
            _ => return None,
        };
        // Use sanitize_audio_target so device_key matches what query() validates
        self.sanitize_audio_target(params, keys, "device_key")
            .ok()
            .flatten()
            .map(Self::normalize_device_key)
            .map(|target| format!("{AUDIO_DEVICE_PREFIX}{target}"))
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match self.action_from_params(&params)? {
            "list" => self.list_devices().await,
            "capture" => self.capture_audio(&params).await,
            "playback" => self.playback_audio(&params).await,
            "volume" => self.volume(&params).await,
            "grant_capture_consent" => self.grant_capture_consent(&params).await,
            "revoke_capture_consent" => self.revoke_capture_consent(&params).await,
            "list_capture_consents" => self.list_capture_consents().await,
            action => Err(AgentOSError::HalError(format!(
                "Unsupported audio action '{action}'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::PermissionSet;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct FakeRunner {
        responses: Mutex<HashMap<String, CommandResult>>,
    }

    impl FakeRunner {
        fn new(responses: HashMap<String, CommandResult>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl AudioCommandRunner for FakeRunner {
        async fn run(&self, program: &str, args: &[String]) -> Result<CommandResult, AgentOSError> {
            let key = format!("{program} {}", args.join(" "));
            self.responses
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .ok_or_else(|| AgentOSError::HalError(format!("unexpected command: {key}")))
        }
    }

    fn success(stdout: &str) -> CommandResult {
        CommandResult {
            status_code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[tokio::test]
    async fn list_devices_parses_sources_and_sinks() {
        let output = r#"
id 47, type PipeWire:Interface:Node/3
    object.serial = "47"
    node.name = "alsa_input.usb-1234"
    node.description = "USB Microphone"
    media.class = "Audio/Source"
id 62, type PipeWire:Interface:Node/3
    object.serial = "62"
    node.name = "alsa_output.pci-0000"
    node.description = "Built-in Audio"
    media.class = "Audio/Sink"
"#;
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([(
            "pw-cli ls Node".to_string(),
            success(output),
        )]))));

        let result = driver
            .list_devices()
            .await
            .expect("device list should parse");
        assert_eq!(result["source_count"], 1);
        assert_eq!(result["sink_count"], 1);
        assert_eq!(result["sources"][0]["device_id"], "audio:47");
        assert_eq!(result["sinks"][0]["device_id"], "audio:62");
    }

    #[tokio::test]
    async fn capture_requires_explicit_consent() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));
        let error = driver
            .capture_audio(&json!({
                "action": "capture",
                "source": "47",
            }))
            .await
            .expect_err("capture should require consent");

        match error {
            AgentOSError::PermissionDenied { resource, .. } => {
                assert_eq!(resource, "hardware.audio.capture.consent");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dynamic_permissions_match_audio_actions() {
        let driver = AudioDriver::new();
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "list" })),
            ("hardware.audio.list", PermissionOp::Read)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "capture" })),
            ("hardware.audio.capture", PermissionOp::Execute)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "playback" })),
            ("hardware.audio.playback", PermissionOp::Execute)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "volume", "volume": 0.4 })),
            ("hardware.audio.volume", PermissionOp::Write)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "volume" })),
            ("hardware.audio.volume", PermissionOp::Read)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "grant_capture_consent" })),
            ("hardware.audio.capture", PermissionOp::Execute)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "list_capture_consents" })),
            ("hardware.audio.capture", PermissionOp::Read)
        );
    }

    #[tokio::test]
    async fn consent_lifecycle_grant_then_revoke() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));

        // Before consent: capture denied
        let err = driver
            .capture_audio(&json!({ "source": "47" }))
            .await
            .expect_err("should require consent");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));

        // Grant consent
        let grant = driver
            .grant_capture_consent(&json!({ "source": "47", "ttl_seconds": 60 }))
            .await
            .expect("grant should succeed");
        assert_eq!(grant["consent_granted"], true);
        assert_eq!(grant["source"], "47");

        // List consents — should have one entry
        let list = driver
            .list_capture_consents()
            .await
            .expect("list should succeed");
        assert_eq!(list["consents"].as_array().unwrap().len(), 1);

        // Revoke consent
        let revoke = driver
            .revoke_capture_consent(&json!({ "source": "47" }))
            .await
            .expect("revoke should succeed");
        assert_eq!(revoke["consent_revoked"], true);

        // After revoke: capture denied again
        let err = driver
            .capture_audio(&json!({ "source": "47" }))
            .await
            .expect_err("should require consent after revoke");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));

        // List consents — should be empty
        let list = driver
            .list_capture_consents()
            .await
            .expect("list should succeed");
        assert!(list["consents"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn consent_ttl_expiration() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));

        // Manually insert an already-expired consent
        {
            let now = Utc::now();
            let mut store = driver.consent_store.write().await;
            store.insert(
                "47".to_string(),
                AudioConsentGrant {
                    target: "47".to_string(),
                    granted_at: now - chrono::Duration::seconds(120),
                    expires_at: now - chrono::Duration::seconds(1),
                },
            );
        }

        // Capture should fail — consent is expired
        let err = driver
            .capture_audio(&json!({ "source": "47" }))
            .await
            .expect_err("expired consent should be rejected");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn consent_ttl_validation() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));

        // TTL of 0 is rejected
        let err = driver
            .grant_capture_consent(&json!({ "source": "47", "ttl_seconds": 0 }))
            .await
            .expect_err("ttl 0 should be rejected");
        assert!(err.to_string().contains("ttl_seconds"));

        // TTL exceeding max is rejected
        let err = driver
            .grant_capture_consent(&json!({ "source": "47", "ttl_seconds": 9999 }))
            .await
            .expect_err("ttl exceeding max should be rejected");
        assert!(err.to_string().contains("ttl_seconds"));
    }

    #[tokio::test]
    async fn volume_get_parses_wpctl_output() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([(
            "wpctl get-volume 62".to_string(),
            success("Volume: 0.74"),
        )]))));

        let result = driver
            .get_volume(&json!({ "node_id": "62" }))
            .await
            .expect("volume query should succeed");
        assert_eq!(result["node_id"], "62");
        assert!((result["volume"].as_f64().unwrap() - 0.74).abs() < f64::EPSILON);
        assert_eq!(result["muted"], false);
    }

    #[tokio::test]
    async fn volume_get_detects_muted() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([(
            "wpctl get-volume 62".to_string(),
            success("Volume: 0.00 [MUTED]"),
        )]))));

        let result = driver
            .get_volume(&json!({ "node_id": "62" }))
            .await
            .expect("volume query should succeed");
        assert_eq!(result["muted"], true);
    }

    #[tokio::test]
    async fn volume_set_calls_wpctl() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([(
            "wpctl set-volume 62 0.50".to_string(),
            success(""),
        )]))));

        let result = driver
            .set_volume(&json!({ "node_id": "62", "volume": 0.5 }))
            .await
            .expect("volume set should succeed");
        assert_eq!(result["updated"], true);
        assert_eq!(result["node_id"], "62");
    }

    #[tokio::test]
    async fn volume_rejects_out_of_range() {
        let driver = AudioDriver::new();
        let err = driver
            .set_volume(&json!({ "node_id": "62", "volume": 2.0 }))
            .await
            .expect_err("volume > 1.5 should be rejected");
        assert!(err.to_string().contains("volume"));

        let err = driver
            .set_volume(&json!({ "node_id": "62", "volume": -0.1 }))
            .await
            .expect_err("negative volume should be rejected");
        assert!(err.to_string().contains("volume"));
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));
        let err = driver
            .query(json!({ "action": "dance" }))
            .await
            .expect_err("unknown action should fail");
        assert!(err.to_string().contains("Unsupported audio action"));
    }

    #[tokio::test]
    async fn sanitize_rejects_leading_hyphen() {
        let driver = AudioDriver::new();
        let err = driver
            .sanitize_audio_target(&json!({ "source": "--verbose" }), &["source"], "source")
            .expect_err("leading hyphen should be rejected");
        assert!(err.to_string().contains("must not start with '-'"));
    }

    #[tokio::test]
    async fn sanitize_rejects_special_characters() {
        let driver = AudioDriver::new();
        let err = driver
            .sanitize_audio_target(&json!({ "source": "node;rm" }), &["source"], "source")
            .expect_err("semicolon should be rejected");
        assert!(err.to_string().contains("unsupported characters"));
    }

    #[tokio::test]
    async fn capture_duration_validation() {
        let driver = AudioDriver::new();
        let err = driver
            .capture_duration_from_params(&json!({ "duration_seconds": 0 }))
            .expect_err("duration 0 should be rejected");
        assert!(err.to_string().contains("duration_seconds"));

        let err = driver
            .capture_duration_from_params(&json!({ "duration_seconds": 999 }))
            .expect_err("duration > MAX should be rejected");
        assert!(err.to_string().contains("duration_seconds"));
    }

    #[tokio::test]
    async fn sample_rate_rejects_overflow() {
        let driver = AudioDriver::new();
        // Value that would silently truncate via `as u32`
        let err = driver
            .sample_rate_from_params(&json!({ "sample_rate": (u64::from(u32::MAX) + 9000) }))
            .expect_err("overflow sample rate should be rejected");
        assert!(err.to_string().contains("sample_rate"));
    }

    #[tokio::test]
    async fn channels_rejects_overflow() {
        let driver = AudioDriver::new();
        let err = driver
            .channels_from_params(&json!({ "channels": (u64::from(u32::MAX) + 2) }))
            .expect_err("overflow channels should be rejected");
        assert!(err.to_string().contains("channels"));
    }

    #[tokio::test]
    async fn output_path_rejects_traversal() {
        let driver = AudioDriver::new();
        let err = driver
            .output_path_from_params(&json!({ "output_path": "/tmp/../etc/evil.wav" }))
            .expect_err("path traversal should be rejected");
        assert!(err.to_string().contains("path traversal"));
    }

    #[tokio::test]
    async fn output_path_rejects_relative() {
        let driver = AudioDriver::new();
        let err = driver
            .output_path_from_params(&json!({ "output_path": "relative/file.wav" }))
            .expect_err("relative path should be rejected");
        assert!(err.to_string().contains("absolute path"));
    }

    #[tokio::test]
    async fn device_key_maps_correctly() {
        let driver = AudioDriver::new();
        assert_eq!(
            driver.device_key(&json!({ "action": "capture", "source": "47" })),
            Some("audio:47".to_string())
        );
        assert_eq!(
            driver.device_key(&json!({ "action": "playback", "sink": "62" })),
            Some("audio:62".to_string())
        );
        assert_eq!(
            driver.device_key(&json!({ "action": "volume", "node_id": "99" })),
            Some("audio:99".to_string())
        );
        assert_eq!(driver.device_key(&json!({ "action": "list" })), None);
    }

    #[tokio::test]
    async fn hal_enforces_action_specific_permissions_for_audio() {
        let mut hal = crate::hal::HardwareAbstractionLayer::new();
        hal.register(Box::new(AudioDriver::with_runner(Arc::new(
            FakeRunner::new(HashMap::new()),
        ))));

        let perms = PermissionSet::new();
        let error = hal
            .query(
                "audio",
                json!({ "action": "capture", "source": "47" }),
                &perms,
                None,
                None,
            )
            .await
            .expect_err("capture should be denied without capture permission");

        match error {
            AgentOSError::PermissionDenied {
                resource,
                operation,
            } => {
                assert_eq!(resource, "hardware.audio.capture");
                assert_eq!(operation, "x");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
