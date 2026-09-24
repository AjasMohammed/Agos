use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use tokio::process::Command;
use uuid::Uuid;

use crate::consent::ConsentStore;
use crate::drivers::audio_sessions::{
    self, PlaybackState, Player, PlayerCommand, PlayerSpawner, SessionRegistry, SystemPlayer,
    Transcoder,
};
use crate::hal::HalDriver;

const DEFAULT_CAPTURE_SECONDS: u64 = 5;
const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u32 = 2;
const MAX_CAPTURE_SECONDS: u64 = 300;
const MAX_SAMPLE_RATE: u32 = 192_000;
const MAX_CHANNELS: u32 = 8;
const MAX_PLAYBACK_BYTES: u64 = 100 * 1024 * 1024;
const DEFAULT_PLAYBACK_SECONDS: u64 = 300;
const MAX_PLAYBACK_SECONDS: u64 = 3_600;
const AUDIO_DEVICE_PREFIX: &str = "audio:";
/// wpctl's alias for the current default output. `volume`/`mute` without a
/// target use it, like `playback` does — "mute the audio" should not need a
/// `list` round-trip first. Gated as `audio:default` (see `device_key`).
const DEFAULT_SINK: &str = "@DEFAULT_AUDIO_SINK@";
/// GNU `timeout` reports 124 whenever it had to fire, regardless of how the
/// child then exited — so 124 means "we stopped it", never "it failed".
const TIMEOUT_FIRED_EXIT_CODE: i32 = 124;
/// Slack a `wait: true` playback allows past its own duration cap before it
/// gives up waiting and reports the last known state.
const PLAYBACK_WAIT_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
struct AudioNode {
    object_id: String,
    /// Canonical device address: the `id NN,` header value, which is what
    /// `wpctl` accepts. The pw-cat paths need `resolve_target_name` instead.
    node_id: String,
    name: String,
    description: String,
    media_class: String,
    /// Informational only: PipeWire's `object.serial`, reported so operators can
    /// correlate with `pw-cli` dumps. Never used to address a node.
    object_serial: Option<String>,
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
            "object_serial": self.object_serial,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CommandResult {
    status_code: i32,
    stdout: String,
    stderr: String,
}

/// Run-to-completion helper commands (`pw-cli`, `wpctl`, `ffmpeg`).
///
/// Kept separate from [`PlayerSpawner`]: one concrete runner implements both
/// and the driver coerces it into an `Arc` of each, so neither trait needs the
/// other and a test double still has to fake both surfaces.
#[async_trait]
pub(crate) trait AudioCommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[String]) -> Result<CommandResult, AgentOSError>;
}

pub(crate) struct SystemAudioCommandRunner;

#[async_trait]
impl PlayerSpawner for SystemAudioCommandRunner {
    async fn spawn(&self, program: &str, args: &[String]) -> Result<Box<dyn Player>, AgentOSError> {
        Ok(Box::new(SystemPlayer::spawn(program, args).await?))
    }
}

/// Decode a container libsndfile cannot read into a WAV `pw-play` accepts.
struct FfmpegTranscoder {
    runner: Arc<dyn AudioCommandRunner>,
}

#[async_trait]
impl Transcoder for FfmpegTranscoder {
    async fn to_wav(&self, input: &Path, output: &Path) -> Result<(), AgentOSError> {
        let args = vec![
            "-v".to_string(),
            "error".to_string(),
            "-nostdin".to_string(),
            "-y".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-f".to_string(),
            "wav".to_string(),
            output.display().to_string(),
        ];
        let result = self.runner.run("ffmpeg", &args).await?;
        if result.status_code != 0 {
            let detail = result.stderr.trim();
            let detail = if detail.is_empty() {
                "ffmpeg failed without diagnostic output"
            } else {
                detail
            };
            return Err(AgentOSError::HalError(detail.to_string()));
        }
        Ok(())
    }
}

#[async_trait]
impl AudioCommandRunner for SystemAudioCommandRunner {
    async fn run(&self, program: &str, args: &[String]) -> Result<CommandResult, AgentOSError> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A helper that outlives the kernel has nothing left to stop it —
            // `pw-record` would keep the microphone open with its consent TTL
            // no longer being checked.
            .kill_on_drop(true)
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
    /// Consent grants keyed `(authenticated agent, "audio:<node>")`. Shared
    /// with the kernel so operator device approval can grant a capture window.
    consent_store: Arc<ConsentStore>,
    runner: Arc<dyn AudioCommandRunner>,
    spawner: Arc<dyn PlayerSpawner>,
    transcoder: Arc<dyn Transcoder>,
    /// Live playback sessions. The driver is registered once per kernel, so
    /// this map is the process-wide view of what is currently audible.
    sessions: SessionRegistry,
}

impl Default for AudioDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDriver {
    pub fn new() -> Self {
        Self::with_consent_store(Arc::new(ConsentStore::new()))
    }

    /// Construct with a shared consent store. The kernel passes its own store
    /// so that `agentos hal approve audio:<node> <agent>` grants the capture
    /// consent window the driver checks.
    pub fn with_consent_store(consent_store: Arc<ConsentStore>) -> Self {
        Self::assemble(consent_store, Arc::new(SystemAudioCommandRunner))
    }

    /// One concrete runner, two trait objects. Taking it generically (rather
    /// than as `Arc<dyn AudioCommandRunner>`) is what lets it coerce into both
    /// without trait upcasting.
    fn assemble<R: AudioCommandRunner + PlayerSpawner + 'static>(
        consent_store: Arc<ConsentStore>,
        runner: Arc<R>,
    ) -> Self {
        let spawner: Arc<dyn PlayerSpawner> = runner.clone();
        let runner: Arc<dyn AudioCommandRunner> = runner;
        Self {
            consent_store,
            spawner,
            transcoder: Arc::new(FfmpegTranscoder {
                runner: Arc::clone(&runner),
            }),
            runner,
            sessions: SessionRegistry::default(),
        }
    }

    #[cfg(test)]
    fn with_runner<R: AudioCommandRunner + PlayerSpawner + 'static>(runner: Arc<R>) -> Self {
        Self::assemble(Arc::new(ConsentStore::new()), runner)
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
        // A present non-string target (models often emit `"node_id": 50`)
        // must not read as "missing": volume/mute/playback would then drive
        // the default sink under the `audio:default` grant instead.
        if keys.iter().any(|key| {
            params
                .get(*key)
                .is_some_and(|v| !v.is_string() && !v.is_null())
        }) {
            return Err(AgentOSError::HalError(format!(
                "Invalid '{field_name}' param: must be a string"
            )));
        }
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
        let key = target.strip_prefix(AUDIO_DEVICE_PREFIX).unwrap_or(target);
        // wpctl parses ids with strtol, so "49", "049" and "49abc" all address
        // node 49 — but they are three distinct HardwareRegistry keys, which
        // would let a denied or quarantined "audio:49" be retried as
        // "audio:049" and re-prompt the operator as if it were a new device.
        // Canonicalise anything that is a plain number so the gate key and the
        // node wpctl actually drives cannot diverge.
        match key.parse::<u32>() {
            Ok(id) => id.to_string(),
            Err(_) => key.to_string(),
        }
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

        // The tool wrapper stamps the agent's own `captures/` dir under
        // `__output_dir`; `/tmp` only when driven without the wrapper (tests).
        let dir = params
            .get("__output_dir")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Ok(dir.join(format!("agentos-audio-{}.wav", Uuid::new_v4())))
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
        self.run_expecting(program, args, error_context, &[0]).await
    }

    async fn run_expecting(
        &self,
        program: &str,
        args: &[String],
        error_context: &str,
        ok_codes: &[i32],
    ) -> Result<CommandResult, AgentOSError> {
        let result = self.runner.run(program, args).await?;
        if !ok_codes.contains(&result.status_code) {
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
        // `pw-cli ls Node` indents every node header with a leading tab
        // ("\tid 49, type PipeWire:Interface:Node/3"), so the leading \s* is
        // load-bearing: without it no header ever matched, current_id stayed
        // None, and every node was dropped.
        static ID_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^\s*id\s+(\d+),").expect("valid regex"));
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

            // PipeWire has TWO node namespaces and the tools disagree:
            //   wpctl              -> object id (the `id NN,` header)
            //   pw-cat --target    -> object.serial OR node.name (never the id)
            // Keying on object.serial addressed the wrong node in wpctl (on a
            // live host the sink was id 49 / serial 50, and wpctl 50 was the
            // *microphone*), so the canonical key here is the object id and
            // `resolve_target_name` converts it for the pw-cat paths. The
            // serial is reported for operator correlation only.
            let node_id = object_id.clone();
            let object_serial = fields.get("object.serial").cloned();
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
                object_serial,
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

    /// `wpctl` addresses nodes by the object id from the `id NN,` header, but
    /// `pw-cat --target` (i.e. pw-play / pw-record) accepts only an
    /// `object.serial` or a `node.name` — never an object id. Verified on
    /// PipeWire 1.0.3: `--target <object id>` matches no node's serial and
    /// falls back to the *default* device, silently and with exit code 0, so a
    /// capture approved for one node records another. Resolve to `node.name`,
    /// which is stable across daemon restarts (ids and serials are not) and so
    /// cannot be re-pointed at a different device by a restart between the
    /// consent check and the recording.
    async fn resolve_target_name(&self, device_key: &str) -> Result<String, AgentOSError> {
        let output = self
            .run_checked(
                "pw-cli",
                &["ls".to_string(), "Node".to_string()],
                "PipeWire device enumeration failed",
            )
            .await?;
        self.parse_pw_cli_nodes(&output.stdout)?
            .into_iter()
            .find(|node| node.node_id == device_key)
            .map(|node| node.name)
            .ok_or_else(|| AgentOSError::HalError(format!("Audio node '{device_key}' not found")))
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

    /// Consent resource key for a capture source — identical to the registry
    /// device key (`audio:<node>`), so an operator `agentos hal approve` and
    /// the consent check use the same identifier.
    fn consent_resource(source: &str) -> String {
        format!(
            "{AUDIO_DEVICE_PREFIX}{}",
            Self::normalize_device_key(source)
        )
    }

    /// The authenticated agent identity stamped into the payload by the tool
    /// wrapper (`AudioTool`). Agent-supplied `agent_id`/`session_id` claims
    /// are never consulted — only the kernel-injected reserved key counts.
    fn authenticated_agent(params: &Value) -> Result<&str, AgentOSError> {
        params
            .get("__authenticated_agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AgentOSError::HalError(
                    "This audio action requires an authenticated agent identity".into(),
                )
            })
    }

    /// Consent grants are operator-originated (`agentos hal approve`); an
    /// agent must never be able to grant or revoke its own capture consent.
    fn consent_is_operator_only() -> Result<Value, AgentOSError> {
        Err(AgentOSError::PermissionDenied {
            resource: "hardware.audio.capture.consent".to_string(),
            operation: "operator_approval_required".to_string(),
        })
    }

    async fn list_capture_consents(&self) -> Result<Value, AgentOSError> {
        let entries: Vec<Value> = self
            .consent_store
            .list()
            .into_iter()
            .filter(|(_, resource, _)| resource.starts_with(AUDIO_DEVICE_PREFIX))
            .map(|(agent_id, resource, ttl_seconds)| {
                json!({
                    "agent_id": agent_id,
                    "source": resource,
                    "ttl_seconds": ttl_seconds,
                })
            })
            .collect();

        Ok(json!({ "consents": entries }))
    }

    fn ensure_capture_consent(&self, agent_id: &str, source: &str) -> Result<(), AgentOSError> {
        if self
            .consent_store
            .check(agent_id, &Self::consent_resource(source))
        {
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

        let agent_id = Self::authenticated_agent(params)?;
        self.ensure_capture_consent(agent_id, source)?;

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
            self.resolve_target_name(&Self::normalize_device_key(source))
                .await?,
        ];
        // No `--remote` passthrough: capture always targets the local
        // PipeWire daemon. An agent-supplied remote would widen the capture
        // surface beyond the device the operator approved.
        // Positional output path must come last, after all flags
        args.push(output_path.display().to_string());

        // `timeout` IS the stop mechanism here: it always fires at
        // `duration_seconds`, so 124 is the success path. The real proof of a
        // good capture is the non-empty output file checked just below.
        self.run_expecting(
            "timeout",
            &args,
            "PipeWire audio capture failed",
            &[0, TIMEOUT_FIRED_EXIT_CODE],
        )
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
            // Surfaced so the kernel's ToolExecuted audit records that this
            // capture passed an operator-granted consent check.
            "consent_checked": true,
            "audio_path": output_path.display().to_string(),
            "duration_seconds": duration_seconds,
            "sample_rate": sample_rate,
            "channels": channels,
            "format": "wav",
            "source": Self::normalize_device_key(source),
        }))
    }

    fn playback_limit_from_params(&self, params: &Value) -> Result<u64, AgentOSError> {
        let seconds = params
            .get("max_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_PLAYBACK_SECONDS);
        if seconds == 0 || seconds > MAX_PLAYBACK_SECONDS {
            return Err(AgentOSError::HalError(format!(
                "'max_seconds' must be between 1 and {MAX_PLAYBACK_SECONDS}"
            )));
        }
        Ok(seconds)
    }

    /// `pw-play` argv without the trailing input path, so the supervisor can
    /// re-use it verbatim when it has to replay a transcoded copy.
    async fn playback_flags(&self, sink: Option<&str>) -> Result<Vec<String>, AgentOSError> {
        let mut flags = vec![
            "--media-type".to_string(),
            "Audio".to_string(),
            "--media-category".to_string(),
            "Playback".to_string(),
            "--media-role".to_string(),
            "Notification".to_string(),
        ];
        if let Some(sink) = sink {
            flags.push("--target".to_string());
            flags.push(
                self.resolve_target_name(&Self::normalize_device_key(sink))
                    .await?,
            );
        }
        Ok(flags)
    }

    /// Start a supervised playback session and return its handle immediately.
    ///
    /// Playback used to run to completion inside this call, parking the calling
    /// agent's turn for the length of the track. The session is registered here
    /// and driven by a supervisor task; `playback_pause` / `playback_resume` /
    /// `playback_stop` / `playback_status` act on the returned `playback_id`.
    async fn playback_audio(&self, params: &Value) -> Result<Value, AgentOSError> {
        let audio_path = self.playback_path_from_params(params).await?;
        let sink = self.sanitize_audio_target(params, &["sink", "node_id"], "sink")?;
        let max_seconds = self.playback_limit_from_params(params)?;
        let agent_id = Self::authenticated_agent(params)?;

        let flags = self.playback_flags(sink).await?;
        let mut args = flags.clone();
        args.push(audio_path.display().to_string());

        // Spawned here rather than inside the supervisor on purpose: a missing
        // `pw-play`, a dead PipeWire socket or an EACCES has to fail the tool
        // call the agent is waiting on, not become background state it would
        // have to poll for.
        let player = self.spawner.spawn("pw-play", &args).await?;

        // The concurrency cap lives in the registry so the check and the
        // insert share one lock. A player refused here is dropped on return
        // and `kill_on_drop` stops it.
        let playback_id = audio_sessions::start_session(
            &self.sessions,
            Arc::clone(&self.spawner),
            Arc::clone(&self.transcoder),
            agent_id,
            audio_path.clone(),
            sink.map(Self::normalize_device_key),
            max_seconds,
            player,
            flags,
        )?;

        // `wait` restores the old blocking semantics for short notification
        // sounds, where handling a session id costs more than the sound is
        // worth. Everything else returns now and is controlled by handle.
        if params.get("wait").and_then(Value::as_bool).unwrap_or(false) {
            return self
                .await_playback(&playback_id, agent_id, max_seconds)
                .await;
        }

        Ok(json!({
            "started": true,
            "playback_id": playback_id,
            "state": PlaybackState::Playing.as_str(),
            "audio_path": audio_path.display().to_string(),
            "sink": sink.map(Self::normalize_device_key),
            "max_seconds": max_seconds,
        }))
    }

    /// Block until a session reaches a terminal state, then report it.
    ///
    /// Bounded independently of the supervisor: the supervisor enforces the
    /// duration cap, but if its task ever died the completion signal would
    /// never fire and this call — which exists to keep a turn short — would
    /// hang it forever. On expiry the last known state is returned, not an
    /// error; the track may well still be playing.
    async fn await_playback(
        &self,
        id: &str,
        agent_id: &str,
        max_seconds: u64,
    ) -> Result<Value, AgentOSError> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(max_seconds) + PLAYBACK_WAIT_GRACE;
        loop {
            let Some(done) = self.sessions.completion(id, agent_id) else {
                return self.sessions.get_owned(id, agent_id);
            };
            // `Notified` does NOT join the waiter list at construction — it
            // registers on first poll — and `notify_waiters` stores no permit.
            // Without the explicit `enable()` a completion firing between this
            // line and the poll below is missed, and the caller waits out the
            // whole timeout for a track that already ended.
            let mut notified = std::pin::pin!(done.notified());
            notified.as_mut().enable();
            let snapshot = self.sessions.get_owned(id, agent_id)?;
            if snapshot
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| !matches!(state, "playing" | "paused"))
            {
                return Ok(snapshot);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                tracing::warn!(
                    playback_id = %id,
                    "Playback supervisor did not report completion within the duration cap"
                );
                return self.sessions.get_owned(id, agent_id);
            }
        }
    }

    fn playback_id_from_params(params: &Value) -> Result<Option<String>, AgentOSError> {
        match params.get("playback_id") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(id)) if !id.trim().is_empty() => Ok(Some(id.trim().to_string())),
            Some(_) => Err(AgentOSError::HalError(
                "Invalid 'playback_id' param: expected a non-empty session id".into(),
            )),
        }
    }

    /// Pause / resume / stop a session the calling agent owns.
    ///
    /// Ownership is the authorization here: the operator approved this agent
    /// driving this sink when the session started, and these actions can only
    /// ever narrow what that already-approved player is doing.
    async fn playback_control(
        &self,
        params: &Value,
        command: PlayerCommand,
    ) -> Result<Value, AgentOSError> {
        let agent_id = Self::authenticated_agent(params)?;
        match Self::playback_id_from_params(params)? {
            Some(id) => self.sessions.command(&id, agent_id, command).await,
            // "stop the music" rarely arrives with a session id. Scoped to the
            // caller's own sessions, so a bare stop can never reach another
            // agent's playback.
            None if command == PlayerCommand::Stop => {
                let ids = self.sessions.active_owned_ids(agent_id);
                if ids.is_empty() {
                    return Err(AgentOSError::HalError(
                        "No playback sessions are running".into(),
                    ));
                }
                let mut stopped = Vec::new();
                for id in ids {
                    // A session that ended on its own between the listing and
                    // the send is not a failure of the stop.
                    if self
                        .sessions
                        .command(&id, agent_id, PlayerCommand::Stop)
                        .await
                        .is_ok()
                    {
                        stopped.push(id);
                    }
                }
                Ok(json!({ "stopped": stopped.len(), "playback_ids": stopped }))
            }
            None => Err(AgentOSError::HalError("Missing 'playback_id' param".into())),
        }
    }

    /// One session by id, or every session this agent owns when no id is given.
    fn playback_status(&self, params: &Value) -> Result<Value, AgentOSError> {
        let agent_id = Self::authenticated_agent(params)?;
        match Self::playback_id_from_params(params)? {
            Some(id) => self.sessions.get_owned(&id, agent_id),
            None => {
                let playbacks = self.sessions.list_owned(agent_id);
                Ok(json!({ "count": playbacks.len(), "playbacks": playbacks }))
            }
        }
    }

    async fn get_volume(&self, params: &Value) -> Result<Value, AgentOSError> {
        let node_id = self
            .sanitize_audio_target(params, &["node_id", "sink", "source"], "node_id")?
            .unwrap_or(DEFAULT_SINK);
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

    /// Set a node's volume, clearing its mute flag when the target is audible.
    ///
    /// `wpctl set-volume` leaves the mute flag alone, so raising a muted sink
    /// used to report `updated: true` and stay silent — and nothing in the
    /// result said why. A caller asking for a non-zero volume is asking to
    /// hear something, so the mute is cleared as part of the same action.
    /// `volume: 0.0` leaves the flag untouched; explicit muting is what
    /// `action: "mute"` is for.
    async fn set_volume(&self, params: &Value) -> Result<Value, AgentOSError> {
        let node_id = self
            .sanitize_audio_target(params, &["node_id", "sink", "source"], "node_id")?
            .unwrap_or(DEFAULT_SINK);
        let volume = self.volume_from_params(params)?;
        let device = Self::normalize_device_key(node_id);
        // Everything below reads the *rounded* level, never the raw param.
        // `0.001` is sent to wpctl as `0.00`, so deciding the unmute (or
        // reporting the level back) from the unrounded value would claim an
        // audible node that is in fact silent — the exact failure this
        // function was changed to stop reporting.
        let level = format!("{volume:.2}");
        let applied = level.parse::<f64>().unwrap_or(volume);
        let args = vec!["set-volume".to_string(), device.clone(), level];
        self.run_checked("wpctl", &args, "PipeWire volume update failed")
            .await?;

        // After the level, so a failure here cannot leave a node unmuted at a
        // volume the caller never got to set.
        let unmuted = applied > 0.0;
        if unmuted {
            let args = vec!["set-mute".to_string(), device.clone(), "0".to_string()];
            self.run_checked(
                "wpctl",
                &args,
                // Name the half that already landed: the caller otherwise has
                // to re-read the node to find out whether the level changed.
                &format!("PipeWire mute update failed (volume was set to {applied:.2})"),
            )
            .await?;
        }

        Ok(json!({
            "updated": true,
            "node_id": device,
            "volume": applied,
            // `muted` too, with the same polarity `mute` and `get_volume` use,
            // so one key reads the same across all three actions.
            "muted": !unmuted,
            "unmuted": unmuted,
        }))
    }

    async fn volume(&self, params: &Value) -> Result<Value, AgentOSError> {
        if params.get("volume").is_some() {
            self.set_volume(params).await
        } else {
            self.get_volume(params).await
        }
    }

    /// `wpctl set-volume` does not clear a mute flag, so muting/unmuting needs
    /// its own `wpctl set-mute` call. Omit `muted` to read the current state.
    async fn mute(&self, params: &Value) -> Result<Value, AgentOSError> {
        let node_id = self
            .sanitize_audio_target(params, &["node_id", "sink", "source"], "node_id")?
            .unwrap_or(DEFAULT_SINK);

        let Some(desired) = params.get("muted") else {
            return self.get_volume(params).await;
        };
        // Only a real bool toggles mute — a string like "toggle" would be
        // forwarded to wpctl verbatim and flip state unpredictably.
        let desired = desired.as_bool().ok_or_else(|| {
            AgentOSError::HalError(
                "'muted' must be a boolean (true to mute, false to unmute)".into(),
            )
        })?;

        let args = vec![
            "set-mute".to_string(),
            Self::normalize_device_key(node_id),
            if desired { "1" } else { "0" }.to_string(),
        ];
        self.run_checked("wpctl", &args, "PipeWire mute update failed")
            .await?;

        Ok(json!({
            "updated": true,
            "node_id": Self::normalize_device_key(node_id),
            "muted": desired,
        }))
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
            // Every lifecycle action, `playback_status` included, rides the
            // same `playback:x` grant that started the session. A separate
            // read permission to see back your own playback would be friction
            // with no security value: sessions are owner-scoped, so there is
            // nothing to read that this agent did not itself start.
            "playback" | "playback_pause" | "playback_resume" | "playback_stop"
            | "playback_status" => ("hardware.audio.playback", PermissionOp::Execute),
            "volume" => {
                if params.get("volume").is_some() {
                    ("hardware.audio.volume", PermissionOp::Write)
                } else {
                    ("hardware.audio.volume", PermissionOp::Read)
                }
            }
            "mute" => {
                if params.get("muted").is_some() {
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
        // `missing_is_default`: `playback`, `volume` and `mute` treat the
        // target as OPTIONAL and fall back to the PipeWire default sink, so a
        // missing target must still produce a key — otherwise driving the
        // default speakers skipped the approval gate entirely.
        let (keys, missing_is_default): (&[&str], bool) = match action {
            "capture" | "grant_capture_consent" | "revoke_capture_consent" => {
                (&["source", "node_id"], false)
            }
            "playback" => (&["sink", "node_id"], true),
            "volume" | "mute" => (&["node_id", "sink", "source"], true),
            // Lifecycle actions address a session handle, never a device: the
            // sink was gated when `playback` started it, and the caller must
            // already own the session. Returning a key here would demand
            // approval for a device the call does not touch.
            "playback_pause" | "playback_resume" | "playback_stop" | "playback_status" => {
                return None
            }
            _ => return None,
        };
        // Use sanitize_audio_target so device_key matches what query() validates
        match self
            .sanitize_audio_target(params, keys, "device_key")
            .ok()
            .flatten()
        {
            Some(target) => Some(format!(
                "{AUDIO_DEVICE_PREFIX}{}",
                Self::normalize_device_key(target)
            )),
            None if missing_is_default => Some(format!("{AUDIO_DEVICE_PREFIX}default")),
            None => None,
        }
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match self.action_from_params(&params)? {
            "list" => self.list_devices().await,
            "capture" => self.capture_audio(&params).await,
            "playback" => self.playback_audio(&params).await,
            "playback_pause" => self.playback_control(&params, PlayerCommand::Pause).await,
            "playback_resume" => self.playback_control(&params, PlayerCommand::Resume).await,
            "playback_stop" => self.playback_control(&params, PlayerCommand::Stop).await,
            "playback_status" => self.playback_status(&params),
            "volume" => self.volume(&params).await,
            "mute" => self.mute(&params).await,
            "grant_capture_consent" | "revoke_capture_consent" => Self::consent_is_operator_only(),
            "list_capture_consents" => self.list_capture_consents().await,
            action => Err(AgentOSError::HalError(format!(
                "Unsupported audio action '{action}'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {

    /// Verbatim `pw-cli ls Node` capture (PipeWire 1.0.3). The leading tabs are
    /// load-bearing: a hand-typed unindented version passed for months while the
    /// parser matched nothing in production. Sink = header id 49 / serial 50,
    /// source = header id 50 / serial 51 — the two namespaces deliberately
    /// disagree so tests can tell which one a code path uses.
    const PW_CLI_NODES: &str = "\n\tid 49, type PipeWire:Interface:Node/3\n \t\tobject.serial = \"50\"\n \t\tobject.path = \"alsa:pcm:1:front:1:playback\"\n \t\tdevice.id = \"47\"\n \t\tnode.description = \"Family 17h (Models 10h-1fh) HD Audio Controller Analog Stereo\"\n \t\tnode.name = \"alsa_output.pci-0000_04_00.6.analog-stereo\"\n \t\tnode.nick = \"CX11880 Analog\"\n \t\tmedia.class = \"Audio/Sink\"\n\tid 50, type PipeWire:Interface:Node/3\n \t\tobject.serial = \"51\"\n \t\tobject.path = \"alsa:pcm:1:front:1:capture\"\n \t\tdevice.id = \"47\"\n \t\tnode.description = \"Family 17h (Models 10h-1fh) HD Audio Controller Analog Stereo\"\n \t\tnode.name = \"alsa_input.pci-0000_04_00.6.analog-stereo\"\n \t\tnode.nick = \"CX11880 Analog\"\n \t\tmedia.class = \"Audio/Source\"\n\tid 51, type PipeWire:Interface:Node/3\n \t\tnode.name = \"Midi-Bridge\"\n \t\tmedia.class = \"Midi/Bridge\"\n";

    use super::*;
    use crate::drivers::audio_sessions::{
        PlayerExit, PlayerSignal, PlayerSignaller, MAX_PLAYBACKS_PER_AGENT,
    };
    use agentos_types::PermissionSet;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Scripted player processes for the playback tests.
    ///
    /// Playback is now supervised, so a test double has to model a *process*,
    /// not a command line: it records the signals the supervisor delivers, and
    /// exits when interrupted the way `pw-play` does on SIGINT.
    #[derive(Default)]
    struct PlayerScript {
        /// Every `pw-play` argv the driver spawned, in order.
        spawns: Mutex<Vec<String>>,
        /// Signals delivered, tagged with the spawn index they reached — a
        /// test running two agents has two players, and each must only see its
        /// own SIGINT.
        signals: Mutex<Vec<(usize, PlayerSignal)>>,
        /// Exit handed to the Nth spawned player; absent = plays until stopped.
        exits: Mutex<VecDeque<PlayerExit>>,
        /// Woken whenever a signal lands, so a blocked `wait` can re-check.
        signalled: Arc<tokio::sync::Notify>,
    }

    impl PlayerScript {
        fn spawns(&self) -> Vec<String> {
            self.spawns.lock().unwrap().clone()
        }

        /// Signals delivered to the player at `index`.
        fn signals_for(&self, index: usize) -> Vec<PlayerSignal> {
            self.signals
                .lock()
                .unwrap()
                .iter()
                .filter(|(target, _)| *target == index)
                .map(|(_, signal)| *signal)
                .collect()
        }

        /// Every signal delivered, for tests with a single player.
        fn signals(&self) -> Vec<PlayerSignal> {
            self.signals
                .lock()
                .unwrap()
                .iter()
                .map(|(_, signal)| *signal)
                .collect()
        }
    }

    struct FakeSignaller {
        index: usize,
        script: Arc<PlayerScript>,
    }

    impl PlayerSignaller for FakeSignaller {
        fn signal(&self, signal: PlayerSignal) -> Result<(), AgentOSError> {
            self.script
                .signals
                .lock()
                .unwrap()
                .push((self.index, signal));
            self.script.signalled.notify_waiters();
            Ok(())
        }
    }

    struct FakePlayer {
        index: usize,
        script: Arc<PlayerScript>,
        exit: Option<PlayerExit>,
    }

    #[async_trait]
    impl Player for FakePlayer {
        fn signaller(&self) -> Arc<dyn PlayerSignaller> {
            Arc::new(FakeSignaller {
                index: self.index,
                script: Arc::clone(&self.script),
            })
        }

        async fn wait(&mut self) -> PlayerExit {
            if let Some(exit) = self.exit.take() {
                return exit;
            }
            // Otherwise behave like pw-play: play until interrupted.
            loop {
                let signalled = self.script.signalled.notified();
                if self
                    .script
                    .signals_for(self.index)
                    .contains(&PlayerSignal::Interrupt)
                {
                    return PlayerExit {
                        code: 130,
                        stderr: String::new(),
                    };
                }
                signalled.await;
            }
        }
    }

    struct FakeRunner {
        responses: Mutex<HashMap<String, CommandResult>>,
        /// Every `run` command line, in order. `spawn` lines land in the
        /// script instead, so the two are told apart at the assertion.
        calls: Mutex<Vec<String>>,
        /// Result for a command with no scripted response. `None` echoes the
        /// command line back as an error — several tests read the argv that way.
        fallback: Option<CommandResult>,
        players: Arc<PlayerScript>,
    }

    impl FakeRunner {
        fn new(responses: HashMap<String, CommandResult>) -> Self {
            Self {
                responses: Mutex::new(responses),
                calls: Mutex::new(Vec::new()),
                fallback: None,
                players: Arc::new(PlayerScript::default()),
            }
        }

        /// Answer any unscripted command with success instead of erroring.
        fn permissive(mut self) -> Self {
            self.fallback = Some(CommandResult {
                status_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            });
            self
        }

        /// Hand `exit` to the next player spawned.
        fn player_exits(self, exit: PlayerExit) -> Self {
            self.players.exits.lock().unwrap().push_back(exit);
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl AudioCommandRunner for FakeRunner {
        async fn run(&self, program: &str, args: &[String]) -> Result<CommandResult, AgentOSError> {
            let key = format!("{program} {}", args.join(" "));
            self.calls.lock().unwrap().push(key.clone());
            if let Some(scripted) = self.responses.lock().unwrap().get(&key) {
                return Ok(scripted.clone());
            }
            self.fallback
                .clone()
                .ok_or_else(|| AgentOSError::HalError(format!("unexpected command: {key}")))
        }
    }

    #[async_trait]
    impl PlayerSpawner for FakeRunner {
        async fn spawn(
            &self,
            program: &str,
            args: &[String],
        ) -> Result<Box<dyn Player>, AgentOSError> {
            let index = {
                let mut spawns = self.players.spawns.lock().unwrap();
                spawns.push(format!("{program} {}", args.join(" ")));
                spawns.len() - 1
            };
            let exit = self.players.exits.lock().unwrap().pop_front();
            Ok(Box::new(FakePlayer {
                index,
                script: Arc::clone(&self.players),
                exit,
            }))
        }
    }

    /// Poll `check` until it holds. Lifecycle commands cross an mpsc into the
    /// supervisor task, so the state the test asserts on settles a beat after
    /// the call that requested it returns.
    async fn eventually(label: &str, mut check: impl FnMut() -> bool) {
        for _ in 0..400 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for: {label}");
    }

    fn playback_params(path: &Path, agent: &str) -> Value {
        json!({
            "audio_path": path.to_str().unwrap(),
            "__authenticated_agent_id": agent,
        })
    }

    /// A driver whose helper commands all succeed and whose players play until
    /// stopped, plus the script recording what reached them.
    fn playback_driver() -> (AudioDriver, Arc<FakeRunner>) {
        let runner = Arc::new(
            FakeRunner::new(HashMap::from([(
                "pw-cli ls Node".to_string(),
                success(PW_CLI_NODES),
            )]))
            .permissive(),
        );
        (AudioDriver::with_runner(Arc::clone(&runner)), runner)
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
        let output = PW_CLI_NODES;
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
        // device_id must key off the `id NN` header (what wpctl accepts), NOT
        // object.serial. Sink = header 49 / serial 50, source = header 50 /
        // serial 51 — so a serial-keyed device_id would address the source as
        // "audio:50" and silently drive the microphone.
        assert_eq!(result["sinks"][0]["device_id"], "audio:49");
        assert_eq!(result["sources"][0]["device_id"], "audio:50");
        assert_eq!(result["sinks"][0]["object_serial"], "50");
        // Midi/Bridge must not be counted as audio.
        assert_eq!(result["sinks"].as_array().unwrap().len(), 1);
    }

    /// C1 regression. `pw-cat --target` accepts only object.serial or node.name;
    /// handing it an object id matches nothing and PipeWire silently falls back
    /// to the DEFAULT device with exit code 0. Verified on PipeWire 1.0.3:
    /// `pw-record --target 49` (the sink's object id) linked the microphone. So
    /// a capture consented for one node would record another, undetectably —
    /// the only signal is which string reaches argv, hence this test.
    #[tokio::test]
    async fn capture_targets_node_name_not_object_id() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([(
            "pw-cli ls Node".to_string(),
            success(PW_CLI_NODES),
        )]))));
        driver
            .consent_store
            .grant("agent-a", "audio:50", std::time::Duration::from_secs(60));

        let msg = driver
            .capture_audio(&json!({
                "source": "50",
                "__authenticated_agent_id": "agent-a",
            }))
            .await
            .expect_err("FakeRunner echoes the command line it received")
            .to_string();

        assert!(
            msg.contains("--target alsa_input.pci-0000_04_00.6.analog-stereo"),
            "must target by node.name: {msg}"
        );
        // The bare object id and the serial must BOTH be absent as the target:
        // 50 is the source's object id and also the sink's serial, so leaking
        // either would point pw-record at the wrong device.
        assert!(
            !msg.contains("--target 50") && !msg.contains("--target 51"),
            "numeric ids must never reach --target: {msg}"
        );
    }

    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn playback_targets_node_name_not_object_id() {
        let (driver, runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");

        let mut params = playback_params(file.path(), "agent-a");
        params["sink"] = json!("49");
        driver
            .playback_audio(&params)
            .await
            .expect("playback should start");

        let spawns = runner.players.spawns();
        assert_eq!(spawns.len(), 1, "{spawns:?}");
        assert!(
            spawns[0].contains("--target alsa_output.pci-0000_04_00.6.analog-stereo"),
            "must target the sink by node.name: {spawns:?}"
        );
        assert!(
            !spawns[0].contains("--target 49") && !spawns[0].contains("--target 50"),
            "numeric ids must never reach --target: {spawns:?}"
        );
    }

    /// The reported bug: the agent's turn was parked for the whole track
    /// because playback ran to completion inside the tool call. The call must
    /// return a handle while the player keeps playing.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn playback_returns_a_handle_while_the_track_keeps_playing() {
        let (driver, runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");

        let started = driver
            .playback_audio(&playback_params(file.path(), "agent-a"))
            .await
            .expect("playback should start");

        assert_eq!(started["state"], "playing");
        let id = started["playback_id"].as_str().expect("a session handle");
        assert!(!id.is_empty());
        // The player was spawned and is still running: nothing signalled it.
        assert_eq!(runner.players.spawns().len(), 1);
        assert!(runner.players.signals().is_empty());

        let status = driver
            .playback_status(&json!({
                "__authenticated_agent_id": "agent-a",
                "playback_id": id,
            }))
            .expect("status for a session we own");
        assert_eq!(status["state"], "playing");
        assert_eq!(status["max_seconds"], DEFAULT_PLAYBACK_SECONDS);
    }

    /// pw-play decodes via libsndfile, which rejects MP3/AAC/M4A with
    /// "Format not recognised" — the agent-visible symptom reported from the
    /// Nemo3 chat. The supervisor must transcode once and replay, not surface
    /// it. Triggered by the error, never the extension.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn playback_transcodes_when_libsndfile_rejects_format() {
        let runner = Arc::new(
            FakeRunner::new(HashMap::from([(
                "pw-cli ls Node".to_string(),
                success(PW_CLI_NODES),
            )]))
            .permissive()
            .player_exits(PlayerExit {
                code: 1,
                stderr: "sndfile: failed to open audio file: Format not recognised.".into(),
            }),
        );
        let driver = AudioDriver::with_runner(Arc::clone(&runner));
        let dir = tempfile::tempdir().expect("temp dir");
        let mp3 = dir.path().join("song.mp3");
        std::fs::write(&mp3, b"ID3").expect("write");

        let started = driver
            .playback_audio(&playback_params(&mp3, "agent-a"))
            .await
            .expect("playback should start");
        let id = started["playback_id"].as_str().unwrap().to_string();

        eventually("the transcoded copy to be playing", || {
            runner.players.spawns().len() == 2
        })
        .await;

        let spawns = runner.players.spawns();
        assert!(spawns[0].ends_with(".mp3"), "{spawns:?}");
        assert!(spawns[1].ends_with(".wav"), "{spawns:?}");
        assert!(
            runner
                .calls()
                .iter()
                .any(|call| call.starts_with("ffmpeg ")),
            "the retry must go through ffmpeg: {:?}",
            runner.calls()
        );

        eventually("the session to report the transcode", || {
            driver
                .sessions
                .get_owned(&id, "agent-a")
                .is_ok_and(|session| session["transcoded"] == true)
        })
        .await;
    }

    /// SIGSTOP/SIGCONT is the only pause pw-play has — verified on PipeWire
    /// 1.0.3, where a 12s tone stopped at t=2 and continued at t=7 took 17s
    /// wall-clock, so the audio genuinely halts.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pause_and_resume_signal_stop_then_cont() {
        let (driver, runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");
        let started = driver
            .playback_audio(&playback_params(file.path(), "agent-a"))
            .await
            .expect("playback should start");
        let id = started["playback_id"].as_str().unwrap().to_string();
        let owned = json!({ "__authenticated_agent_id": "agent-a", "playback_id": id });

        // The call waits for the supervisor to apply the signal, so the state
        // it reports is the one the session actually reached — the agent never
        // has to poll to find out whether the pause took.
        let paused = driver
            .playback_control(&owned, PlayerCommand::Pause)
            .await
            .expect("pause");
        assert_eq!(paused["state"], "paused");
        assert_eq!(paused["requested"], "pause");
        assert_eq!(runner.players.signals(), vec![PlayerSignal::Pause]);

        // Pausing twice would deliver a second SIGSTOP and hide the real state.
        driver
            .playback_control(&owned, PlayerCommand::Pause)
            .await
            .expect_err("already paused");

        let resumed = driver
            .playback_control(&owned, PlayerCommand::Resume)
            .await
            .expect("resume");
        assert_eq!(resumed["state"], "playing");
        assert_eq!(
            runner.players.signals(),
            vec![PlayerSignal::Pause, PlayerSignal::Resume]
        );
        assert_eq!(
            driver.sessions.get_owned(&id, "agent-a").unwrap()["state"],
            "playing"
        );
    }

    /// A SIGSTOPped process does not act on SIGINT until it is continued, so
    /// stopping a paused track has to wake it first — otherwise the stop is
    /// silently ignored and the track resumes later.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stopping_a_paused_track_continues_it_before_interrupting() {
        let (driver, runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");
        let started = driver
            .playback_audio(&playback_params(file.path(), "agent-a"))
            .await
            .expect("playback should start");
        let id = started["playback_id"].as_str().unwrap().to_string();
        let owned = json!({ "__authenticated_agent_id": "agent-a", "playback_id": id });

        let paused = driver
            .playback_control(&owned, PlayerCommand::Pause)
            .await
            .expect("pause");
        assert_eq!(paused["state"], "paused");

        let stopped = driver
            .playback_control(&owned, PlayerCommand::Stop)
            .await
            .expect("stop");
        assert_eq!(stopped["state"], "stopped");

        assert_eq!(
            runner.players.signals(),
            vec![
                PlayerSignal::Pause,
                PlayerSignal::Resume,
                PlayerSignal::Interrupt
            ],
            "a paused player must be continued before SIGINT can reach it"
        );
    }

    /// "Stop the music" rarely arrives with a session id, so a bare stop hits
    /// everything — but only what the caller started.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_bare_stop_only_reaches_the_callers_own_sessions() {
        let (driver, _runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");

        let mine = driver
            .playback_audio(&playback_params(file.path(), "agent-a"))
            .await
            .expect("start");
        let theirs = driver
            .playback_audio(&playback_params(file.path(), "agent-b"))
            .await
            .expect("start");
        let mine = mine["playback_id"].as_str().unwrap().to_string();
        let theirs = theirs["playback_id"].as_str().unwrap().to_string();

        let stopped = driver
            .playback_control(
                &json!({ "__authenticated_agent_id": "agent-a" }),
                PlayerCommand::Stop,
            )
            .await
            .expect("bare stop");
        assert_eq!(stopped["stopped"], 1);
        assert_eq!(
            driver.sessions.get_owned(&mine, "agent-a").unwrap()["state"],
            "stopped"
        );
        assert_eq!(
            driver.sessions.get_owned(&theirs, "agent-b").unwrap()["state"],
            "playing",
            "another agent's playback must be untouched"
        );
        assert!(
            _runner.players.signals_for(1).is_empty(),
            "no signal may reach the other agent's player"
        );
    }

    /// Ownership — not the approval prompt — is what authorises the lifecycle
    /// actions, so it has to hold even for a caller that guesses a valid id.
    /// A session that is not yours reports as absent rather than forbidden, so
    /// ids cannot be probed to enumerate another agent's activity.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn another_agent_can_neither_see_nor_control_a_session() {
        let (driver, runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");
        let started = driver
            .playback_audio(&playback_params(file.path(), "agent-a"))
            .await
            .expect("start");
        let id = started["playback_id"].as_str().unwrap().to_string();
        let intruder = json!({ "__authenticated_agent_id": "agent-b", "playback_id": id });

        let error = driver
            .playback_control(&intruder, PlayerCommand::Stop)
            .await
            .expect_err("not the owner")
            .to_string();
        assert!(error.contains("No playback session"), "{error}");
        driver
            .playback_status(&intruder)
            .expect_err("not the owner");

        assert!(
            runner.players.signals().is_empty(),
            "no signal may reach a player the caller does not own"
        );
        let listed = driver
            .playback_status(&json!({ "__authenticated_agent_id": "agent-b" }))
            .expect("an empty list, not an error");
        assert_eq!(listed["count"], 0);
    }

    /// Every live session is audible at once, so an agent looping on playback
    /// is a pile-up. The refusal has to name what must be stopped.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_playback_is_capped() {
        let (driver, _runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");

        let mut started = Vec::new();
        for _ in 0..MAX_PLAYBACKS_PER_AGENT {
            let session = driver
                .playback_audio(&playback_params(file.path(), "agent-a"))
                .await
                .expect("under the cap");
            started.push(session["playback_id"].as_str().unwrap().to_string());
        }
        let error = driver
            .playback_audio(&playback_params(file.path(), "agent-a"))
            .await
            .expect_err("over the cap")
            .to_string();
        assert!(error.contains("stop one first"), "{error}");
        for id in &started {
            assert!(
                error.contains(id),
                "the refusal must name every session to stop; {id} missing from: {error}"
            );
        }
    }

    /// The cap refusal names sessions so the agent knows what to stop — which
    /// must not turn it into an oracle for another agent's activity. Session
    /// ids are owner-scoped everywhere else; an error message is not an
    /// exemption.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_cap_refusal_never_names_another_agents_sessions() {
        let (driver, _runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");

        let mut theirs = Vec::new();
        for _ in 0..MAX_PLAYBACKS_PER_AGENT {
            let session = driver
                .playback_audio(&playback_params(file.path(), "agent-b"))
                .await
                .expect("agent-b fills its own quota");
            theirs.push(session["playback_id"].as_str().unwrap().to_string());
        }

        // Its own quota is untouched, so agent-a can still start a track.
        driver
            .playback_audio(&playback_params(file.path(), "agent-a"))
            .await
            .expect("one agent's sessions must not consume another's quota");

        for _ in 1..MAX_PLAYBACKS_PER_AGENT {
            driver
                .playback_audio(&playback_params(file.path(), "agent-a"))
                .await
                .expect("under its own cap");
        }
        let error = driver
            .playback_audio(&playback_params(file.path(), "agent-a"))
            .await
            .expect_err("over its own cap")
            .to_string();
        for id in &theirs {
            assert!(
                !error.contains(id),
                "another agent's session id leaked into the refusal: {error}"
            );
        }
    }

    /// The whole point of the pause accounting: a track paused for longer than
    /// its budget must not be killed on resume, and the time must not be
    /// reported as played.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_paused_span_counts_as_neither_played_nor_elapsed() {
        let (driver, _runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");

        let mut params = playback_params(file.path(), "agent-a");
        params["max_seconds"] = json!(1);
        let started = driver.playback_audio(&params).await.expect("start");
        let id = started["playback_id"].as_str().unwrap().to_string();
        let owned = json!({ "__authenticated_agent_id": "agent-a", "playback_id": id });

        driver
            .playback_control(&owned, PlayerCommand::Pause)
            .await
            .expect("pause");
        // Longer than the whole 1s budget. A cap that kept running would have
        // truncated the track by now.
        tokio::time::sleep(Duration::from_millis(1_500)).await;

        let paused = driver.sessions.get_owned(&id, "agent-a").unwrap();
        assert_eq!(
            paused["state"], "paused",
            "the cap must not fire while paused"
        );
        assert_eq!(
            paused["played_seconds"], 0,
            "paused time is not played time: {paused}"
        );

        let resumed = driver
            .playback_control(&owned, PlayerCommand::Resume)
            .await
            .expect("resume");
        assert_eq!(resumed["state"], "playing");
    }

    /// A track cut short by `max_seconds` ends `truncated`, not `failed` — the
    /// cap is how playback stops, so it is a success path.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_duration_cap_truncates_rather_than_fails() {
        let (driver, runner) = playback_driver();
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");

        let mut params = playback_params(file.path(), "agent-a");
        params["max_seconds"] = json!(1);
        let started = driver.playback_audio(&params).await.expect("start");
        let id = started["playback_id"].as_str().unwrap().to_string();

        eventually("the cap to fire", || {
            driver.sessions.get_owned(&id, "agent-a").unwrap()["state"] == "truncated"
        })
        .await;
        assert!(
            runner.players.signals().contains(&PlayerSignal::Interrupt),
            "the cap stops the player with SIGINT"
        );
        assert_eq!(
            driver.sessions.get_owned(&id, "agent-a").unwrap()["error"],
            Value::Null,
            "hitting the cap is not an error"
        );
    }

    /// Time spent paused must not count against `max_seconds`, or pausing a
    /// track for longer than its remaining budget silently kills it on resume.
    #[test]
    fn a_paused_span_extends_the_duration_cap() {
        let start = tokio::time::Instant::now();
        let deadline = start + Duration::from_secs(30);
        let extended = audio_sessions::deadline_after_pause(deadline, Duration::from_secs(45));
        assert_eq!(extended - start, Duration::from_secs(75));
    }

    /// `wait: true` keeps the old blocking behaviour for short notification
    /// sounds: the call returns only once the track is done.
    // Multi-threaded on purpose: the supervisor must be able to finish on
    // another worker while this task is mid-call, which is the only way the
    // notification and reap races are reachable at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wait_true_blocks_until_the_track_finishes() {
        let runner = Arc::new(
            FakeRunner::new(HashMap::from([(
                "pw-cli ls Node".to_string(),
                success(PW_CLI_NODES),
            )]))
            .permissive()
            .player_exits(PlayerExit {
                code: 0,
                stderr: String::new(),
            }),
        );
        let driver = AudioDriver::with_runner(runner);
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"RIFF").expect("write");

        let mut params = playback_params(file.path(), "agent-a");
        params["wait"] = json!(true);
        let result = driver.playback_audio(&params).await.expect("start");
        assert_eq!(result["state"], "finished");
        assert_eq!(result["error"], Value::Null);
    }

    #[tokio::test]
    async fn playback_rejects_out_of_range_max_seconds() {
        let driver = AudioDriver::new();
        for bad in [0u64, MAX_PLAYBACK_SECONDS + 1] {
            driver
                .playback_limit_from_params(&json!({ "max_seconds": bad }))
                .expect_err("out of range");
        }
        assert_eq!(
            driver
                .playback_limit_from_params(&json!({}))
                .expect("default"),
            DEFAULT_PLAYBACK_SECONDS
        );
    }

    /// `timeout` is how capture STOPS pw-record, so it fires on every healthy
    /// capture and GNU timeout then reports 124 no matter how the child exited.
    /// Treating that as failure made every full-duration capture return an
    /// error while the wav sat complete on disk.
    #[tokio::test]
    async fn capture_treats_timeout_exit_as_the_normal_stop() {
        struct CaptureRunner;
        #[async_trait]
        impl AudioCommandRunner for CaptureRunner {
            async fn run(
                &self,
                program: &str,
                _args: &[String],
            ) -> Result<CommandResult, AgentOSError> {
                if program == "pw-cli" {
                    return Ok(CommandResult {
                        status_code: 0,
                        stdout: PW_CLI_NODES.to_string(),
                        stderr: String::new(),
                    });
                }
                Ok(CommandResult {
                    status_code: 124,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }

        // Capture spawns nothing: it is still a run-to-completion command.
        #[async_trait]
        impl PlayerSpawner for CaptureRunner {
            async fn spawn(
                &self,
                _program: &str,
                _args: &[String],
            ) -> Result<Box<dyn Player>, AgentOSError> {
                unreachable!("capture never spawns a player")
            }
        }

        let driver = AudioDriver::with_runner(Arc::new(CaptureRunner));
        driver
            .consent_store
            .grant("agent-a", "audio:50", std::time::Duration::from_secs(60));
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("cap.wav");
        std::fs::write(&out, b"RIFF....").expect("write");

        let result = driver
            .capture_audio(&json!({
                "source": "50",
                "duration_seconds": 2,
                "output_path": out.to_str().unwrap(),
                "__authenticated_agent_id": "agent-a",
            }))
            .await
            .expect("timeout firing is how capture ends");
        assert_eq!(result["captured"], true);
    }

    #[test]
    fn numeric_device_keys_canonicalise_to_one_gate_key() {
        // wpctl's strtol parse makes these the same node; the registry must not
        // see them as different devices or a denial can be laundered.
        for alias in ["audio:049", "049", "audio:49", "49"] {
            assert_eq!(AudioDriver::normalize_device_key(alias), "49", "{alias}");
        }
        // Node names must survive untouched — they are the --target form.
        assert_eq!(
            AudioDriver::normalize_device_key("audio:alsa_output.pci-0000_04_00.6.analog-stereo"),
            "alsa_output.pci-0000_04_00.6.analog-stereo"
        );
    }

    #[tokio::test]
    async fn resolve_target_name_rejects_unknown_node() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([(
            "pw-cli ls Node".to_string(),
            success(PW_CLI_NODES),
        )]))));
        // Must be a hard error, never a fall-through to the default device.
        let err = driver
            .resolve_target_name("9999")
            .await
            .expect_err("unknown node must not silently resolve");
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[tokio::test]
    async fn mute_sets_and_unsets_via_wpctl() {
        let runner = Arc::new(FakeRunner::new(HashMap::from([
            ("wpctl set-mute 50 0".to_string(), success("")),
            ("wpctl set-mute 50 1".to_string(), success("")),
        ])));
        let driver = AudioDriver::with_runner(runner);

        let unmuted = driver
            .mute(&json!({ "node_id": "audio:50", "muted": false }))
            .await
            .expect("unmute should succeed");
        assert_eq!(unmuted["muted"], false);
        assert_eq!(unmuted["node_id"], "50");

        let muted = driver
            .mute(&json!({ "node_id": "audio:50", "muted": true }))
            .await
            .expect("mute should succeed");
        assert_eq!(muted["muted"], true);
    }

    #[tokio::test]
    async fn mute_rejects_non_boolean_muted() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));
        let error = driver
            .mute(&json!({ "node_id": "audio:50", "muted": "toggle" }))
            .await
            .expect_err("non-boolean 'muted' must be rejected before reaching wpctl");
        assert!(error.to_string().contains("must be a boolean"));
    }

    #[tokio::test]
    async fn mute_write_requires_write_permission() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "mute", "muted": false })),
            ("hardware.audio.volume", PermissionOp::Write)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "mute" })),
            ("hardware.audio.volume", PermissionOp::Read)
        );
    }

    #[tokio::test]
    async fn capture_requires_authenticated_identity() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));
        // A payload-supplied agent_id is NOT an authenticated identity.
        let error = driver
            .capture_audio(&json!({
                "action": "capture",
                "source": "47",
                "agent_id": "spoofed",
            }))
            .await
            .expect_err("capture should require authenticated identity");
        assert!(error.to_string().contains("authenticated agent identity"));
    }

    #[tokio::test]
    async fn capture_requires_explicit_consent() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));
        let error = driver
            .capture_audio(&json!({
                "action": "capture",
                "source": "47",
                "__authenticated_agent_id": "agent-a",
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
    async fn agent_cannot_grant_or_revoke_consent() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));
        for action in ["grant_capture_consent", "revoke_capture_consent"] {
            let err = driver
                .query(json!({
                    "action": action,
                    "source": "47",
                    "__authenticated_agent_id": "agent-a",
                }))
                .await
                .expect_err("agent-invoked consent grant/revoke must be rejected");
            match err {
                AgentOSError::PermissionDenied { operation, .. } => {
                    assert_eq!(operation, "operator_approval_required");
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn consent_is_scoped_to_the_granted_agent() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));

        // Operator-originated grant for agent-a on audio:47.
        driver
            .consent_store
            .grant("agent-a", "audio:47", std::time::Duration::from_secs(60));

        // agent-a passes the consent check.
        assert!(driver.ensure_capture_consent("agent-a", "47").is_ok());
        // agent-b does NOT inherit agent-a's grant.
        let err = driver
            .ensure_capture_consent("agent-b", "47")
            .expect_err("another agent must not inherit consent");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));

        // The grant is visible in the listing with its agent.
        let list = driver
            .list_capture_consents()
            .await
            .expect("list should succeed");
        let consents = list["consents"].as_array().unwrap();
        assert_eq!(consents.len(), 1);
        assert_eq!(consents[0]["agent_id"], "agent-a");
        assert_eq!(consents[0]["source"], "audio:47");
    }

    #[tokio::test]
    async fn capture_ignores_agent_supplied_remote() {
        // FakeRunner errors with the exact command line it received, so the
        // assertion can prove `--remote` never reaches pw-record.
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([(
            "pw-cli ls Node".to_string(),
            success(PW_CLI_NODES),
        )]))));
        driver
            .consent_store
            .grant("agent-a", "audio:50", std::time::Duration::from_secs(60));

        let err = driver
            .capture_audio(&json!({
                "action": "capture",
                "source": "50",
                "remote": "evil-remote",
                "__authenticated_agent_id": "agent-a",
            }))
            .await
            .expect_err("FakeRunner has no canned response, so capture errs with the command");
        let msg = err.to_string();
        assert!(
            msg.contains("--target alsa_input.pci-0000_04_00.6.analog-stereo"),
            "capture should target the source by node.name: {msg}"
        );
        assert!(
            !msg.contains("--remote"),
            "agent-supplied remote must be ignored: {msg}"
        );
    }

    #[tokio::test]
    async fn consent_ttl_expiration() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::new())));

        driver
            .consent_store
            .grant("agent-a", "audio:47", std::time::Duration::from_millis(1));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let err = driver
            .ensure_capture_consent("agent-a", "47")
            .expect_err("expired consent should be rejected");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
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
        let runner = Arc::new(FakeRunner::new(HashMap::new()).permissive());
        let driver = AudioDriver::with_runner(Arc::clone(&runner));

        let result = driver
            .set_volume(&json!({ "node_id": "62", "volume": 0.5 }))
            .await
            .expect("volume set should succeed");
        assert_eq!(result["updated"], true);
        assert_eq!(result["node_id"], "62");
        assert!(
            runner
                .calls()
                .contains(&"wpctl set-volume 62 0.50".to_string()),
            "{:?}",
            runner.calls()
        );
    }

    /// Raising the volume also clears the mute flag.
    ///
    /// Regression for 2026-09-19: `wpctl set-volume` leaves mute alone, so an
    /// agent told to "turn the volume up" set 100% on a muted sink, was told
    /// `updated: true`, and the box stayed silent with nothing in the result
    /// to explain it.
    #[tokio::test]
    async fn volume_set_clears_the_mute_flag() {
        let runner = Arc::new(FakeRunner::new(HashMap::new()).permissive());
        let driver = AudioDriver::with_runner(Arc::clone(&runner));

        let result = driver
            .set_volume(&json!({ "volume": 1.0 }))
            .await
            .expect("volume set should succeed");
        assert_eq!(result["unmuted"], true);

        let calls = runner.calls();
        let volume_at = calls
            .iter()
            .position(|c| c == "wpctl set-volume @DEFAULT_AUDIO_SINK@ 1.00")
            .unwrap_or_else(|| panic!("no set-volume call: {calls:?}"));
        let mute_at = calls
            .iter()
            .position(|c| c == "wpctl set-mute @DEFAULT_AUDIO_SINK@ 0")
            .unwrap_or_else(|| panic!("no set-mute call: {calls:?}"));
        assert!(
            volume_at < mute_at,
            "the level must be set before the unmute: {calls:?}"
        );
    }

    /// `volume: 0.0` is not a request to hear anything, so it leaves the mute
    /// flag exactly as it found it — `action: "mute"` owns that state.
    #[tokio::test]
    async fn volume_set_to_zero_leaves_mute_alone() {
        let runner = Arc::new(FakeRunner::new(HashMap::new()).permissive());
        let driver = AudioDriver::with_runner(Arc::clone(&runner));

        let result = driver
            .set_volume(&json!({ "node_id": "62", "volume": 0.0 }))
            .await
            .expect("volume set should succeed");
        assert_eq!(result["unmuted"], false);
        assert_eq!(result["muted"], true);
        assert!(
            !runner.calls().iter().any(|c| c.contains("set-mute")),
            "{:?}",
            runner.calls()
        );
    }

    /// A level that rounds to `0.00` is silent, so it must not be reported as
    /// unmuted — and the result must echo the level that was actually applied,
    /// not the raw param.
    #[tokio::test]
    async fn volume_below_rounding_resolution_is_not_reported_as_audible() {
        let runner = Arc::new(FakeRunner::new(HashMap::new()).permissive());
        let driver = AudioDriver::with_runner(Arc::clone(&runner));

        let result = driver
            .set_volume(&json!({ "node_id": "62", "volume": 0.001 }))
            .await
            .expect("volume set should succeed");
        assert_eq!(result["unmuted"], false);
        assert_eq!(result["volume"], 0.0, "must report the applied level");
        assert!(
            !runner.calls().iter().any(|c| c.contains("set-mute")),
            "{:?}",
            runner.calls()
        );
    }

    /// A mute failure after the level landed must surface as an error that
    /// names the half that succeeded — never `updated: true`.
    #[tokio::test]
    async fn volume_set_reports_a_failed_unmute_and_names_the_applied_level() {
        // `set-mute` is scripted to a non-zero exit, the way a real wpctl
        // failure arrives (an unscripted command errors in the runner itself,
        // before `run_checked` can attach its context).
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([
            ("wpctl set-volume 62 0.80".to_string(), success("")),
            (
                "wpctl set-mute 62 0".to_string(),
                CommandResult {
                    status_code: 1,
                    stdout: String::new(),
                    stderr: "Node 62 not found".into(),
                },
            ),
        ]))));

        let err = driver
            .set_volume(&json!({ "node_id": "62", "volume": 0.8 }))
            .await
            .expect_err("a failed unmute must not report success");
        let msg = err.to_string();
        assert!(msg.contains("mute update failed"), "{msg}");
        assert!(msg.contains("0.80"), "must name the applied level: {msg}");
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
        // Playback without a sink goes to the PipeWire default — still gated.
        assert_eq!(
            driver.device_key(&json!({ "action": "playback", "audio_path": "/tmp/a.wav" })),
            Some("audio:default".to_string())
        );
        // Volume/mute without a target drive the default sink — still gated.
        assert_eq!(
            driver.device_key(&json!({ "action": "mute", "muted": true })),
            Some("audio:default".to_string())
        );
    }

    #[tokio::test]
    async fn volume_set_without_target_uses_default_sink() {
        // Permissive: `set_volume` also clears the mute flag for a non-zero
        // level, so scripting the `set-volume` line alone would fail the run.
        let driver = AudioDriver::with_runner(Arc::new(
            FakeRunner::new(HashMap::from([(
                "wpctl set-volume @DEFAULT_AUDIO_SINK@ 1.00".to_string(),
                success(""),
            )]))
            .permissive(),
        ));

        let result = driver
            .set_volume(&json!({ "volume": 1.0 }))
            .await
            .expect("default-sink volume set should succeed");
        assert_eq!(result["node_id"], "@DEFAULT_AUDIO_SINK@");
    }

    #[tokio::test]
    async fn mute_without_target_uses_default_sink() {
        let driver = AudioDriver::with_runner(Arc::new(FakeRunner::new(HashMap::from([(
            "wpctl set-mute @DEFAULT_AUDIO_SINK@ 1".to_string(),
            success(""),
        )]))));

        let result = driver
            .mute(&json!({ "muted": true }))
            .await
            .expect("default-sink mute should succeed");
        assert_eq!(result["node_id"], "@DEFAULT_AUDIO_SINK@");
    }

    #[tokio::test]
    async fn numeric_target_is_rejected_not_defaulted() {
        let driver = AudioDriver::new();
        let err = driver
            .mute(&json!({ "node_id": 50, "muted": true }))
            .await
            .expect_err("numeric node_id must not fall back to the default sink");
        assert!(err.to_string().contains("must be a string"), "{err}");
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
