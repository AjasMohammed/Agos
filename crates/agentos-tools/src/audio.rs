//! The `audio` tool: a thin wrapper that stamps the caller's identity onto a
//! payload, contains any path in it, and hands it to the HAL audio driver.
//!
//! One action is serviced here rather than by the driver. `action="speak"`
//! synthesises the text through the operator's `[tts]` endpoint (the same core
//! the `speak` tool uses) and then continues as an ordinary `playback` of the
//! file it just wrote, so the driver needs no text-to-speech support and the
//! whole playback lifecycle — `playback_pause` / `_resume` / `_stop` /
//! `_status` — works on the returned handle unchanged.
//!
//! It lives on `audio` and not as a flag on `speak` because of the approval
//! gate: `risk_class_by_action` may only ever *lower* a class (`verify_manifest`
//! rejects anything but `readonly_external`), so a `play` flag on the
//! `write_scoped` `speak` tool could never be gated as the `control_plane`
//! device operation it is. `audio` is already `control_plane`.

use crate::speak::{SpeechFormat, TtsSettings};
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

pub struct AudioTool {
    client: reqwest::Client,
    /// `None` when this instance was built without the kernel's `[tts]` block.
    ///
    /// Deliberately not `TtsSettings::default()`: that would report
    /// `[tts].enabled = false` to an operator who has it set to `true`, sending
    /// them to fix a config that is already correct.
    tts: Option<TtsSettings>,
}

impl AudioTool {
    /// No text-to-speech. Used by `ToolRunner::new` (before the kernel
    /// re-registers a configured one) and by the CLI tool factory, which builds
    /// tools for sandbox children and has no kernel config to pass.
    pub fn new() -> Self {
        Self {
            // Client::new() cannot fail — avoids `.expect()` in production paths.
            client: reqwest::Client::new(),
            tts: None,
        }
    }

    /// Built by the kernel from `[tts]`, exactly like `SpeakTool`. The endpoint
    /// is operator config and never agent input, which is why `action="speak"`
    /// needs no `network.outbound` grant.
    pub fn with_tts(tts: TtsSettings) -> Self {
        Self {
            client: reqwest::Client::new(),
            tts: Some(tts),
        }
    }
}

impl Default for AudioTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for AudioTool {
    fn name(&self) -> &str {
        "audio"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            // `speak` writes one clip into the agent's own home before playing
            // it, so the static union carries the file grant too.
            ("fs.user_data".to_string(), PermissionOp::Write),
            ("hardware.audio.list".to_string(), PermissionOp::Read),
            ("hardware.audio.capture".to_string(), PermissionOp::Read),
            ("hardware.audio.capture".to_string(), PermissionOp::Execute),
            ("hardware.audio.playback".to_string(), PermissionOp::Execute),
            ("hardware.audio.volume".to_string(), PermissionOp::Read),
            ("hardware.audio.volume".to_string(), PermissionOp::Write),
        ]
    }

    fn required_permissions_for(&self, payload: &Value) -> Vec<(String, PermissionOp)> {
        match payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => vec![("hardware.audio.list".to_string(), PermissionOp::Read)],
            "capture" => {
                vec![("hardware.audio.capture".to_string(), PermissionOp::Execute)]
            }
            "list_capture_consents" => {
                vec![("hardware.audio.capture".to_string(), PermissionOp::Read)]
            }
            // Synthesise-and-play is a playback that writes one file first, so
            // it needs both grants. `playback:x` alone would let an agent
            // without `fs.user_data:w` write into its own home through the
            // audio tool; the write alone would drive the speakers on a file
            // grant. The payload is rewritten to `action="playback"` before it
            // reaches the driver, so the driver's own re-check sees
            // `playback:x` and the two cannot disagree.
            "speak" => vec![
                ("hardware.audio.playback".to_string(), PermissionOp::Execute),
                ("fs.user_data".to_string(), PermissionOp::Write),
            ],
            // Mirrors the driver: the lifecycle actions ride the same
            // `playback:x` grant that started the session they address.
            "playback" | "playback_pause" | "playback_resume" | "playback_stop"
            | "playback_status" => {
                vec![("hardware.audio.playback".to_string(), PermissionOp::Execute)]
            }
            "volume" => {
                let op = if payload.get("volume").is_some() {
                    PermissionOp::Write
                } else {
                    PermissionOp::Read
                };
                vec![("hardware.audio.volume".to_string(), op)]
            }
            // Must mirror the driver's own mapping: without this arm "mute"
            // fell through to `audio.list:r`, gating a write on a read grant.
            "mute" => {
                let op = if payload.get("muted").is_some() {
                    PermissionOp::Write
                } else {
                    PermissionOp::Read
                };
                vec![("hardware.audio.volume".to_string(), op)]
            }
            _ => vec![("hardware.audio.list".to_string(), PermissionOp::Read)],
        }
    }

    async fn execute(
        &self,
        payload: Value,
        context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        let hal = context
            .hal
            .clone()
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "Hardware Abstraction Layer (HAL) not available in this context"
                    .to_string(),
            })?;

        // Synthesis runs before anything else so a TTS failure never reaches
        // the HAL, and before the consent check because `speak` can never be a
        // consent action.
        let mut payload = payload;
        let speech = self.synthesize_if_speaking(&mut payload, &context).await?;
        // From here on the clip is on disk. Everything that can still fail —
        // the consent check, path containment, the HAL's own device gate, a
        // missing `pw-play` — has to take it with it, or a call that never made
        // a sound leaves a file nothing will ever read.
        let outcome = self.play(hal, payload, &context).await;
        let mut result = match (outcome, &speech) {
            (Ok(result), _) => result,
            (Err(error), None) => return Err(error),
            (Err(error), Some(speech)) => {
                if let Some(path) = speech.get("__abs").and_then(Value::as_str) {
                    if let Err(cleanup) = tokio::fs::remove_file(path).await {
                        tracing::warn!(
                            tool = self.name(),
                            error = %cleanup,
                            "Could not remove the speech clip of a playback that failed to start"
                        );
                    }
                }
                return Err(error);
            }
        };

        // One call, one result: the playback handle the driver returned plus
        // what was synthesised to produce it.
        if let (Some(Value::Object(mut extra)), Value::Object(map)) = (speech, &mut result) {
            extra.remove("__abs");
            // The driver owns any key it set. Overwriting one here would
            // silently replace a real playback field (the capture action
            // already returns `format`, and the webcam driver `bytes`) with a
            // synthesis value, and nothing would notice.
            extra.retain(|key, _| !map.contains_key(key));
            map.extend(extra);
        }
        Ok(result)
    }
}

impl AudioTool {
    /// The rest of `execute`: gate the payload and hand it to the driver.
    ///
    /// Split out so the speak path has one place to catch a failure that
    /// happens after the clip was written.
    async fn play(
        &self,
        hal: std::sync::Arc<agentos_hal::HardwareAbstractionLayer>,
        payload: Value,
        context: &ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        let mut payload = payload;
        // A turn cancelled during synthesis aborts the fetch, but one cancelled
        // just after it would still drive the speakers. "Cancel" not stopping
        // the thing making noise is the one case an operator always notices.
        if context.cancellation_token.is_cancelled() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "cancelled".to_string(),
            });
        }

        // Consent grants are operator-originated (`agentos hal approve`);
        // an agent must never grant or revoke its own capture consent.
        if let Some(action) = payload.get("action").and_then(Value::as_str) {
            if matches!(action, "grant_capture_consent" | "revoke_capture_consent") {
                return Err(AgentOSError::PermissionDenied {
                    resource: "hardware.audio.capture.consent".to_string(),
                    operation: "operator_approval_required".to_string(),
                });
            }
        }

        // Stamp the authenticated identity into the payload under a reserved
        // key the driver trusts, and strip every agent-supplied identity claim
        // (including an attempt to forge the reserved key itself).
        if let Value::Object(map) = &mut payload {
            // File tools hand out paths relative to the agent home
            // (`file-glob` → `inbox/<id>/song.mp3`); the driver only takes
            // absolute ones and writes wherever it is told.
            crate::workspace::contain_hal_path(map, "audio_path", self.name(), context, false)?;
            crate::workspace::contain_hal_path(map, "output_path", self.name(), context, true)?;
            // Recordings with no `output_path` land in the agent home, not /tmp.
            let capture_dir = crate::workspace::agent_capture_dir(self.name(), context)?;
            map.insert(
                crate::workspace::HAL_OUTPUT_DIR_KEY.to_string(),
                Value::String(capture_dir.to_string_lossy().into_owned()),
            );
            map.remove("agent_id");
            map.remove("session_id");
            map.insert(
                "__authenticated_agent_id".to_string(),
                Value::String(context.agent_id.to_string()),
            );
        }

        // Forward the agent's real grant — the kernel validated the token
        // against the payload-scoped permissions, so the HAL-internal check
        // re-verifies the same authority instead of a self-minted set.
        hal.query(
            "audio",
            payload,
            &context.permissions,
            Some(&context.agent_id),
            Some(&context.task_id),
        )
        .await
    }

    /// Service `action="speak"` in the wrapper, rewriting the payload into the
    /// `playback` the driver already understands.
    ///
    /// Returns the synthesis facts to fold into the result, or `None` for every
    /// other action. WAV, not mp3: `pw-play` decodes with libsndfile, which
    /// plays WAV natively and rejects mp3 — an mp3 here costs a failed spawn
    /// and a full ffmpeg transcode before the first sample, and fails outright
    /// wherever ffmpeg is missing.
    async fn synthesize_if_speaking(
        &self,
        payload: &mut Value,
        context: &ToolExecutionContext,
    ) -> Result<Option<Value>, AgentOSError> {
        // Taken before synthesis, not after: every fallible step has to happen
        // while there is still nothing on disk to leak. A non-object payload
        // has no "action" either, so this is also the action check.
        let Some(map) = payload.as_object_mut() else {
            return Ok(None);
        };
        if map.get("action").and_then(Value::as_str) != Some("speak") {
            return Ok(None);
        }
        // Only the kernel-registered instance carries `[tts]`. A sandbox child
        // (kernel.sandbox_policy = "always") gets one from the tool factory
        // instead, which has no config to give it — say so rather than blaming
        // a setting the operator may well have enabled.
        let tts = self
            .tts
            .as_ref()
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: self.name().to_string(),
                reason: "text-to-speech is unavailable in this execution context: this audio tool \
                     was built without the kernel's [tts] configuration, which happens when it \
                     runs in a sandbox child. Play an existing file with action=\"playback\" \
                     instead, and tell the operator."
                    .to_string(),
            })?;
        // Owned before the payload is mutated below: the validated pair borrows
        // from it.
        // Owned, which releases the shared reborrow of `map` and leaves the
        // `&mut` live across the await below.
        let (text, voice) = crate::speak::validate_speech_request(map, tts, self.name())
            .map(|(text, voice)| (text.to_string(), voice.to_string()))?;

        let speech = crate::speak::synthesize_to_file(
            &self.client,
            tts,
            &text,
            &voice,
            SpeechFormat::Wav,
            context,
            self.name(),
        )
        .await?;

        map.insert("action".into(), Value::String("playback".into()));
        map.insert(
            "audio_path".into(),
            Value::String(speech.abs.to_string_lossy().into_owned()),
        );
        // The synthesis inputs have done their job. Left in place they would
        // reach the driver and its audit record as unrecognised keys carrying
        // the spoken text verbatim. (Not the approval preview: `ToolPre` fires
        // in the caller, before `execute`, so the operator sees — and should
        // see — the original `text`.)
        map.remove("text");
        map.remove("voice");

        Ok(Some(serde_json::json!({
            "speech_path": speech.rel,
            "bytes": speech.bytes,
            "voice": voice,
            "format": SpeechFormat::Wav.as_str(),
            // Stripped before the result reaches the agent; carried only so a
            // failure after this point can remove the clip.
            "__abs": speech.abs.to_string_lossy(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_hal::{HalDriver, HardwareAbstractionLayer};
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Stands in for the real audio driver so a test can assert exactly what
    /// the wrapper handed down.
    struct RecordingDriver {
        seen: Arc<Mutex<Vec<Value>>>,
    }

    #[async_trait]
    impl HalDriver for RecordingDriver {
        fn name(&self) -> &str {
            "audio"
        }

        fn required_permission(&self) -> (&str, PermissionOp) {
            ("hardware.audio.playback", PermissionOp::Execute)
        }

        async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
            self.seen.lock().unwrap().push(params);
            Ok(json!({ "started": true, "playback_id": "pb-1" }))
        }
    }

    /// Fails every call, like a missing `pw-play` or a dead PipeWire socket.
    struct FailingDriver;

    #[async_trait]
    impl HalDriver for FailingDriver {
        fn name(&self) -> &str {
            "audio"
        }

        fn required_permission(&self) -> (&str, PermissionOp) {
            ("hardware.audio.playback", PermissionOp::Execute)
        }

        async fn query(&self, _params: Value) -> Result<Value, AgentOSError> {
            Err(AgentOSError::HalError("pw-play not found".into()))
        }
    }

    fn recording_hal() -> (Arc<HardwareAbstractionLayer>, Arc<Mutex<Vec<Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(RecordingDriver { seen: seen.clone() }));
        (Arc::new(hal), seen)
    }

    fn ctx(data_dir: &std::path::Path, hal: Arc<HardwareAbstractionLayer>) -> ToolExecutionContext {
        // The tool forwards the agent's real grant to hal.query, so the test
        // context must actually hold the playback permission.
        let mut permissions = PermissionSet::new();
        permissions.grant_op(
            "hardware.audio.playback".to_string(),
            PermissionOp::Execute,
            None,
        );
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions,
            vault: None,
            hal: Some(hal),
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
            shared_dir: None,
        }
    }

    /// One-shot fake `/audio/speech` answering a WAV body.
    async fn speech_endpoint(body: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !req.ends_with(b"}") {
                let n = sock.read(&mut buf).await.expect("read");
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: audio/wav\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            sock.write_all(head.as_bytes()).await.expect("head");
            let _ = sock.write_all(body).await;
        });
        format!("http://{addr}/v1/audio/speech")
    }

    fn speaking_tool(endpoint: String) -> AudioTool {
        AudioTool::with_tts(TtsSettings {
            enabled: true,
            endpoint,
            // A variable nothing sets: the no-key path is the local-server path.
            api_key_env: "AGENTOS_TEST_TTS_KEY_UNSET".to_string(),
            ..TtsSettings::default()
        })
    }

    #[test]
    fn action_permissions_are_scoped() {
        let tool = AudioTool::new();
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "list" })),
            vec![("hardware.audio.list".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "capture" })),
            vec![("hardware.audio.capture".to_string(), PermissionOp::Execute)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "volume" })),
            vec![("hardware.audio.volume".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "volume", "volume": 0.5 })),
            vec![("hardware.audio.volume".to_string(), PermissionOp::Write)]
        );
        // The wrapper gate is what the KERNEL validates the capability token
        // against; the driver re-checks its own mapping. If these two drift, a
        // write is admitted on a read grant. Without this arm "mute" fell
        // through to the `_` case and was gated on hardware.audio.list:r.
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "mute" })),
            vec![("hardware.audio.volume".to_string(), PermissionOp::Read)]
        );
        assert_eq!(
            tool.required_permissions_for(&json!({ "action": "mute", "muted": false })),
            vec![("hardware.audio.volume".to_string(), PermissionOp::Write)]
        );
        // Same drift trap for the playback lifecycle: falling through to `_`
        // would gate stopping a track on `audio.list:r`, which every agent has.
        for action in [
            "playback",
            "playback_pause",
            "playback_resume",
            "playback_stop",
            "playback_status",
        ] {
            assert_eq!(
                tool.required_permissions_for(&json!({ "action": action })),
                vec![("hardware.audio.playback".to_string(), PermissionOp::Execute)],
                "{action} must ride the playback grant"
            );
        }
    }

    /// The drift trap. Without the `speak` arm the action falls through to `_`
    /// and is gated on `hardware.audio.list:r`, which every agent holds — the
    /// speakers would then be driven under a permission that reads a device
    /// list, and the clip written under no file grant at all.
    #[test]
    fn speak_requires_both_playback_and_file_write() {
        assert_eq!(
            AudioTool::new().required_permissions_for(&json!({ "action": "speak" })),
            vec![
                ("hardware.audio.playback".to_string(), PermissionOp::Execute),
                ("fs.user_data".to_string(), PermissionOp::Write),
            ]
        );
    }

    #[tokio::test]
    async fn speak_is_rewritten_to_playback_before_the_driver_sees_it() {
        let dir = tempfile::tempdir().unwrap();
        let (hal, seen) = recording_hal();
        let endpoint = speech_endpoint(b"RIFF-fake-wav").await;
        let context = ctx(dir.path(), hal);
        let home = context.agent_files_dir().unwrap().canonicalize().unwrap();

        let result = speaking_tool(endpoint)
            .execute(
                json!({ "action": "speak", "text": "hello there", "sink": "audio:49",
                        "max_seconds": 30 }),
                context,
            )
            .await
            .unwrap();

        let calls = seen.lock().unwrap();
        let seen = calls.first().expect("driver was called");
        // The driver has no speak support and needs none.
        assert_eq!(seen["action"], "playback");
        let played = std::path::Path::new(seen["audio_path"].as_str().unwrap());
        assert!(played.is_absolute(), "driver rejects a relative path");
        assert!(played.starts_with(&home), "{played:?} escaped {home:?}");
        assert_eq!(played.extension().unwrap(), "wav");
        assert_eq!(std::fs::read(played).unwrap(), b"RIFF-fake-wav");
        // The spoken text must not ride along into the driver, its audit
        // record or the escalation preview as an unrecognised key.
        assert!(seen.get("text").is_none(), "text leaked to the driver");
        assert!(seen.get("voice").is_none(), "voice leaked to the driver");
        // Everything the playback action already understood survives.
        assert_eq!(seen["sink"], "audio:49");
        assert_eq!(seen["max_seconds"], 30);

        // One call reports both halves: the playback handle and what was said.
        assert_eq!(result["playback_id"], "pb-1");
        assert_eq!(result["started"], true);
        assert_eq!(result["bytes"], 13);
        assert_eq!(result["voice"], "alloy");
        assert_eq!(result["format"], "wav");
        let rel = result["speech_path"].as_str().unwrap();
        assert!(rel.starts_with("speech/") && rel.ends_with(".wav"), "{rel}");
    }

    #[tokio::test]
    async fn a_failed_synthesis_never_reaches_the_driver() {
        let dir = tempfile::tempdir().unwrap();

        // No [tts] at all — the factory-built instance a sandbox child gets.
        // It must not claim the operator disabled text-to-speech: they may well
        // have it enabled, and would be sent to fix a correct config.
        let (hal, seen) = recording_hal();
        let err = AudioTool::new()
            .execute(
                json!({ "action": "speak", "text": "hi" }),
                ctx(dir.path(), hal),
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("unavailable in this execution context"),
            "got {message}"
        );
        assert!(!message.contains("[tts].enabled"), "got {message}");
        assert!(seen.lock().unwrap().is_empty(), "driver was called");

        // [tts].enabled = false — the operator really did turn it off.
        let (hal, seen) = recording_hal();
        let err = AudioTool::with_tts(TtsSettings::default())
            .execute(
                json!({ "action": "speak", "text": "hi" }),
                ctx(dir.path(), hal),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not enabled"), "got {err}");
        assert!(seen.lock().unwrap().is_empty(), "driver was called");

        // Bad input is rejected before the endpoint is even contacted, so an
        // unreachable one is fine here.
        let tool = speaking_tool("http://127.0.0.1:1/unreachable".into());
        for payload in [
            json!({ "action": "speak" }),
            json!({ "action": "speak", "text": "   " }),
            json!({ "action": "speak", "text": "hi", "voice": "../etc" }),
        ] {
            let (hal, seen) = recording_hal();
            let err = tool
                .execute(payload.clone(), ctx(dir.path(), hal))
                .await
                .unwrap_err();
            assert!(
                matches!(err, AgentOSError::SchemaValidation(_)),
                "{payload} → {err}"
            );
            assert!(seen.lock().unwrap().is_empty(), "{payload} reached driver");
        }
    }

    /// Every other action must reach the driver exactly as before — the speak
    /// arm returns early for them and must not touch the payload.
    #[tokio::test]
    async fn other_actions_are_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let (hal, seen) = recording_hal();
        AudioTool::new()
            .execute(json!({ "action": "playback_status" }), ctx(dir.path(), hal))
            .await
            .unwrap();
        let calls = seen.lock().unwrap();
        let seen = calls.first().expect("driver was called");
        assert_eq!(seen["action"], "playback_status");
        assert!(seen.get("text").is_none() && seen.get("__abs").is_none());
    }

    /// An agent that sends `audio_path` alongside `speak` must not get that
    /// file played: the synthesised clip has to win.
    #[tokio::test]
    async fn an_agent_supplied_audio_path_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (hal, seen) = recording_hal();
        let endpoint = speech_endpoint(b"RIFF-fake-wav").await;
        let context = ctx(dir.path(), hal);
        let home = context.agent_files_dir().unwrap().canonicalize().unwrap();

        speaking_tool(endpoint)
            .execute(
                json!({ "action": "speak", "text": "hi", "audio_path": "/etc/hostname" }),
                context,
            )
            .await
            .unwrap();

        let calls = seen.lock().unwrap();
        let played = std::path::Path::new(calls[0]["audio_path"].as_str().unwrap());
        assert!(
            played.starts_with(&home),
            "{played:?} is not the synthesised clip"
        );
        assert_eq!(played.extension().unwrap(), "wav");
    }

    /// A playback that never starts must take its clip with it, or every failed
    /// attempt leaves a WAV in the agent's home that nothing will ever read.
    #[tokio::test]
    async fn a_playback_that_fails_leaves_no_clip() {
        let dir = tempfile::tempdir().unwrap();
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(FailingDriver));
        let endpoint = speech_endpoint(b"RIFF-fake-wav").await;
        let context = ctx(dir.path(), Arc::new(hal));
        let home = context.agent_files_dir().unwrap();

        let err = speaking_tool(endpoint)
            .execute(json!({ "action": "speak", "text": "hi" }), context)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("pw-play not found"), "got {err}");

        let leftovers: Vec<_> = std::fs::read_dir(home.join("speech"))
            .expect("speech/ exists once a clip was written")
            .map(|entry| entry.expect("read speech/ entry").path())
            .collect();
        assert!(leftovers.is_empty(), "clip left behind: {leftovers:?}");
    }

    /// The driver owns the keys it sets. If `playback` ever grows a `bytes` or
    /// `format` field — `capture` already returns `format` — the synthesis
    /// facts must not silently replace it.
    #[tokio::test]
    async fn the_merge_never_clobbers_a_driver_key() {
        struct FormatDriver;

        #[async_trait]
        impl HalDriver for FormatDriver {
            fn name(&self) -> &str {
                "audio"
            }

            fn required_permission(&self) -> (&str, PermissionOp) {
                ("hardware.audio.playback", PermissionOp::Execute)
            }

            async fn query(&self, _params: Value) -> Result<Value, AgentOSError> {
                Ok(json!({ "playback_id": "pb-1", "format": "driver-owned" }))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(FormatDriver));
        let endpoint = speech_endpoint(b"RIFF-fake-wav").await;

        let result = speaking_tool(endpoint)
            .execute(
                json!({ "action": "speak", "text": "hi" }),
                ctx(dir.path(), Arc::new(hal)),
            )
            .await
            .unwrap();

        assert_eq!(result["format"], "driver-owned");
        // The non-colliding facts still arrive.
        assert_eq!(result["bytes"], 13);
        assert!(result["speech_path"].is_string());
        // The internal handoff key must never reach the agent.
        assert!(result.get("__abs").is_none(), "{result}");
    }
}
