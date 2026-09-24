//! Live end-to-end check for `audio { action: "speak" }`.
//!
//! `#[ignore]`d because it needs real hardware and a real speech server: a
//! PipeWire sink with `pw-play` on PATH, and `[tts]` pointed at a reachable
//! OpenAI-compatible `/audio/speech`. Everything it asserts is invisible to the
//! mocked unit tests — that a clip this pipeline produces is one `pw-play`
//! actually decodes, which is the whole reason the playback path asks for WAV
//! rather than the MP3 the `speak` tool writes.
//!
//! It lives in `agentos-kernel` rather than `agentos-tools` because the audio
//! driver is feature-gated (`agentos-hal/audio`) and the kernel is the crate
//! that owns that switch.
//!
//! ```bash
//! AGENTOS_TTS_ENDPOINT=http://localhost:8000/v1/audio/speech \
//! AGENTOS_TTS_MODEL=speaches-ai/Kokoro-82M-v1.0-ONNX-int8 \
//! AGENTOS_TTS_VOICE=af_heart \
//!   cargo test -p agentos-kernel --features audio --test speak_and_play_live \
//!     -- --ignored --nocapture
//! ```

#![cfg(feature = "audio")]

use agentos_hal::HardwareAbstractionLayer;
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_tools::{AudioTool, TtsSettings};
use agentos_types::{AgentID, PermissionOp, PermissionSet, TaskID, TraceID};
use serde_json::json;
use std::sync::Arc;

/// Panics rather than skipping: this test only ever runs when someone asked
/// for it by name with `--ignored`, and a green "pass" that verified no
/// hardware is worse than a loud failure.
fn required_env(key: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => panic!("{key} must be set to run this test — see the module comment"),
    }
}

#[tokio::test]
#[ignore = "needs a real audio sink and a reachable [tts] endpoint"]
async fn speak_reaches_the_speakers() {
    let endpoint = required_env("AGENTOS_TTS_ENDPOINT");
    let dir = tempfile::tempdir().expect("tempdir");

    let mut hal = HardwareAbstractionLayer::new();
    hal.register(Box::new(agentos_hal::drivers::audio::AudioDriver::new()));

    let mut permissions = PermissionSet::new();
    permissions.grant_op(
        "hardware.audio.playback".to_string(),
        PermissionOp::Execute,
        None,
    );
    permissions.grant_op("fs.user_data".to_string(), PermissionOp::Write, None);

    let context = ToolExecutionContext {
        data_dir: dir.path().to_path_buf(),
        task_id: TaskID::new(),
        agent_id: AgentID::new(),
        trace_id: TraceID::new(),
        permissions,
        vault: None,
        hal: Some(Arc::new(hal)),
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
    };

    let tool = AudioTool::with_tts(TtsSettings {
        enabled: true,
        endpoint,
        model: std::env::var("AGENTOS_TTS_MODEL").unwrap_or_else(|_| "tts-1".into()),
        voice: std::env::var("AGENTOS_TTS_VOICE").unwrap_or_else(|_| "alloy".into()),
        api_key_env: "AGENTOS_TTS_KEY".into(),
    });

    // `wait` so the assertion covers the playback actually finishing, not just
    // the session starting — a clip pw-play cannot decode fails here, which is
    // exactly what the mocked tests cannot see.
    let result = tool
        .execute(
            json!({
                "action": "speak",
                "text": "Speak and play works. This is one tool call.",
                "wait": true,
                "max_seconds": 60,
            }),
            context,
        )
        .await
        .expect("speak");

    eprintln!("{}", serde_json::to_string_pretty(&result).expect("json"));
    // "finished" = ran to the end of the track. "failed" is what an
    // undecodable clip produces, "truncated" what max_seconds produces.
    assert_eq!(
        result["state"], "finished",
        "playback did not run to the end: {result}"
    );
    assert_eq!(result["error"], serde_json::Value::Null, "{result}");
    assert!(result["bytes"].as_u64().unwrap_or(0) > 0);
    let rel = result["speech_path"].as_str().expect("speech_path");
    assert!(rel.ends_with(".wav"), "{rel}");
    // A transcode means pw-play could not decode what we asked the endpoint
    // for — the WAV request silently regressed to something it rejects.
    assert_eq!(result["transcoded"], false, "clip needed ffmpeg: {result}");
}
