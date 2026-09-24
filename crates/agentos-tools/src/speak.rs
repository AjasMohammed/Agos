//! Text-to-speech: turn text into an audio file in the agent's own files.
//!
//! Posts to an operator-configured OpenAI-compatible `/audio/speech` endpoint —
//! the mirror of the kernel's inbound `[transcription]`. The destination comes
//! from `[tts]`, never from the payload, which is why this needs no
//! `network.outbound` grant and why the SSRF guard on `http-client` /
//! `web-fetch` (which rightly blocks a loopback `speaches`) stays untouched.
//!
//! The tool only writes the file. Playing it or sending it are separate,
//! separately-gated calls — `channel-send` with `file_path`, or `audio` with
//! `action="speak"`, which runs the synthesis below and the playback in one
//! gated call (see `crate::audio`).
//!
//! Everything below the `SpeakTool` struct is the shared synthesis core: both
//! callers go through `validate_speech_request` and `synthesize_to_file`, so
//! the text cap, the voice charset, the `enabled` gate, the content-type
//! check, the size cap, the error redaction and the cancellation handling
//! cannot drift between them.

use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

/// Longest text accepted in one call — OpenAI's own `/audio/speech` limit is
/// 4096, and compatible servers inherit it.
const MAX_TEXT_CHARS: usize = 4000;
/// How long a synthesised clip stays in `speech/`.
///
/// The file exists to be handed to `pw-play` or `channel-send`; nothing reads
/// it afterwards and nothing else prunes it, so without this every spoken
/// sentence accumulates forever. Comfortably longer than the HAL's 3600s
/// playback cap, so a clip is never swept while it is still playing.
const SPEECH_RETENTION: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Container asked of the speech endpoint.
///
/// `Mp3` is what goes to a chat channel — compact, and every client plays it.
/// `Wav` is what goes to the host speakers: `pw-play` decodes with libsndfile,
/// which plays WAV natively and rejects MP3 with "Format not recognised",
/// costing a failed spawn plus a full ffmpeg transcode before the first sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpeechFormat {
    Mp3,
    Wav,
}

impl SpeechFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
        }
    }

    /// Largest body accepted, per container. 4000 characters is roughly five
    /// minutes of speech: ~1–2 MiB as mp3, but tens of MiB as 44.1 kHz stereo
    /// WAV. The body is streamed to disk, so these bound the file, not memory.
    ///
    /// Both stay under the HAL's `MAX_PLAYBACK_BYTES` (100 MiB), so a file this
    /// accepts is always one `audio` playback will accept.
    fn max_bytes(self) -> u64 {
        match self {
            Self::Mp3 => 10 * 1024 * 1024,
            Self::Wav => 96 * 1024 * 1024,
        }
    }

    /// Budget for the whole request, body read included — reqwest's `timeout`
    /// bounds the entire cycle, not just the headers. It has to scale with
    /// `max_bytes`: 60s was ample for a 10 MiB mp3 and would cut off a large
    /// WAV part-read, reporting it as a request failure with no detail.
    fn request_timeout(self) -> std::time::Duration {
        match self {
            Self::Mp3 => std::time::Duration::from_secs(60),
            Self::Wav => std::time::Duration::from_secs(300),
        }
    }
}

/// `[tts]` — outbound text-to-speech. Disabled by default.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TtsSettings {
    /// Master switch. When false the `speak` tool returns a clear error.
    #[serde(default)]
    pub enabled: bool,
    /// OpenAI-compatible speech endpoint (JSON `model` + `voice` + `input`).
    #[serde(default = "default_tts_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_tts_model")]
    pub model: String,
    /// Voice used when the call names none.
    #[serde(default = "default_tts_voice")]
    pub voice: String,
    /// Environment variable holding the API key (Bearer auth). Never the key
    /// itself. Unset or empty means no `Authorization` header — a local server
    /// needs none.
    #[serde(default = "default_tts_key_env")]
    pub api_key_env: String,
}

impl Default for TtsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_tts_endpoint(),
            model: default_tts_model(),
            voice: default_tts_voice(),
            api_key_env: default_tts_key_env(),
        }
    }
}

fn default_tts_endpoint() -> String {
    "https://api.openai.com/v1/audio/speech".to_string()
}

fn default_tts_model() -> String {
    "tts-1".to_string()
}

fn default_tts_voice() -> String {
    "alloy".to_string()
}

fn default_tts_key_env() -> String {
    "OPENAI_API_KEY".to_string()
}

pub struct SpeakTool {
    client: reqwest::Client,
    settings: TtsSettings,
}

impl SpeakTool {
    pub fn new(settings: TtsSettings) -> Self {
        Self {
            // Client::new() cannot fail — avoids `.expect()` in production paths.
            client: reqwest::Client::new(),
            settings,
        }
    }
}

fn fail(tool_name: &str, reason: impl Into<String>) -> AgentOSError {
    AgentOSError::ToolExecutionFailed {
        tool_name: tool_name.to_string(),
        reason: reason.into(),
    }
}

/// A voice id is an identifier, not free text: it is sent to the server and
/// echoed into results, so it stays in one boring alphabet.
fn valid_voice(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Validated `text` and `voice` for one synthesis request.
///
/// Shared by `speak` and by `audio` `action="speak"` so the character cap and
/// the voice charset cannot differ between the two entry points.
///
/// Takes the payload's map rather than the `Value` so a caller that must then
/// mutate the payload can hold one `&mut` across both, instead of re-borrowing
/// afterwards — a fallible re-borrow placed after synthesis would leak the
/// clip it had already written.
pub(crate) fn validate_speech_request<'a>(
    payload: &'a serde_json::Map<String, serde_json::Value>,
    settings: &'a TtsSettings,
    tool_name: &str,
) -> Result<(&'a str, &'a str), AgentOSError> {
    let text = payload
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AgentOSError::SchemaValidation(format!("{tool_name} requires 'text'")))?;
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(AgentOSError::SchemaValidation(format!(
            "{tool_name}: 'text' is longer than {MAX_TEXT_CHARS} characters — split it into several calls"
        )));
    }
    // `""` is what a model emits for an omitted optional field.
    let voice = payload
        .get("voice")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or(&settings.voice);
    if !valid_voice(voice) {
        return Err(AgentOSError::SchemaValidation(format!(
            "{tool_name}: 'voice' must be 1-64 characters of letters, digits, '_' or '-'"
        )));
    }
    Ok((text, voice))
}

/// Where a synthesised clip landed.
#[derive(Debug)]
pub(crate) struct SpeechFile {
    /// Relative to the agent's own files directory — what the agent is shown.
    pub rel: String,
    /// Absolute — what the HAL playback driver requires.
    pub abs: PathBuf,
    pub bytes: u64,
}

/// Synthesise `text` and write it into the agent's own `speech/` directory.
///
/// The response body is streamed into the destination file rather than
/// buffered, so memory stays at one chunk no matter how large
/// `format.max_bytes()` is — WAV of a 4000-character line is tens of MiB.
/// Every failure path, cancellation included, removes the partial file: a
/// truncated clip left on disk is one the agent could still hand to `playback`.
pub(crate) async fn synthesize_to_file(
    client: &reqwest::Client,
    settings: &TtsSettings,
    text: &str,
    voice: &str,
    format: SpeechFormat,
    context: &ToolExecutionContext,
    tool_name: &str,
) -> Result<SpeechFile, AgentOSError> {
    if !settings.enabled {
        return Err(fail(
            tool_name,
            "text-to-speech is not enabled on this system ([tts].enabled = false). \
             Tell the user; do not retry.",
        ));
    }

    let mut req = client
        .post(&settings.endpoint)
        .timeout(format.request_timeout())
        .json(&serde_json::json!({
            "model": settings.model,
            "voice": voice,
            "input": text,
            "response_format": format.as_str(),
        }));
    let api_key = std::env::var(&settings.api_key_env)
        .ok()
        .map(Zeroizing::new)
        .filter(|k| !k.trim().is_empty());
    if let Some(key) = &api_key {
        req = req.bearer_auth(key.as_str());
    }

    // No payload-derived path component, so nothing to traverse with.
    let speech_dir = speech_dir(context, tool_name).await?;
    // Best-effort, and deliberately before the request: a call that is about to
    // add a file is the only moment anything looks at this directory.
    prune_speech_dir(&speech_dir).await;
    let name = format!("{}.{}", uuid::Uuid::new_v4(), format.as_str());
    let rel = format!("speech/{name}");
    let path = speech_dir.join(&name);

    let fetch = async {
        // Error details are dropped, as in transcription: a reqwest error
        // can carry the full request URL and header context.
        let mut resp = req
            .send()
            .await
            .map_err(|_| fail(tool_name, "speech request failed (details redacted)"))?;
        if !resp.status().is_success() {
            return Err(fail(
                tool_name,
                format!("speech HTTP {}", resp.status().as_u16()),
            ));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !(content_type.starts_with("audio/")
            || content_type.starts_with("application/octet-stream"))
        {
            return Err(fail(tool_name, "speech endpoint did not return audio"));
        }

        // Opened only once the reply is known to be audio, so a refused or
        // non-audio response leaves no file behind, only the empty directory.
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| fail(tool_name, format!("could not write audio file: {e}")))?;
        let mut bytes: u64 = 0;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| fail(tool_name, "speech response read failed"))?
        {
            bytes = bytes.saturating_add(chunk.len() as u64);
            if bytes > format.max_bytes() {
                return Err(fail(
                    tool_name,
                    format!(
                        "speech response exceeds {} MiB",
                        format.max_bytes() / (1024 * 1024)
                    ),
                ));
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| fail(tool_name, format!("could not write audio file: {e}")))?;
        }
        if bytes == 0 {
            return Err(fail(tool_name, "speech endpoint returned no audio"));
        }
        // A buffered tail that never reaches disk would hand the HAL a
        // truncated file that still passes its size and type checks.
        file.flush()
            .await
            .map_err(|e| fail(tool_name, format!("could not write audio file: {e}")))?;
        Ok(bytes)
    };

    let outcome = tokio::select! {
        r = fetch => r,
        _ = context.cancellation_token.cancelled() => Err(fail(tool_name, "cancelled")),
    };
    match outcome {
        Ok(bytes) => Ok(SpeechFile {
            rel,
            abs: path,
            bytes,
        }),
        // One cleanup site for every failure — an early return that skipped it
        // would leave a partial clip behind. Cancellation drops `fetch`
        // mid-write, so this covers a half-written file too: the unlink races a
        // write that may still be in flight, which on Linux leaves the late
        // bytes going to an already-unnamed inode.
        Err(error) => {
            if let Err(cleanup) = tokio::fs::remove_file(&path).await {
                // Not NotFound: the failure happened before the file existed.
                if cleanup.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        tool = tool_name,
                        error = %cleanup,
                        "Could not remove a partial speech clip; it may still be playable"
                    );
                }
            }
            Err(error)
        }
    }
}

/// `<agent home>/speech`, created and verified to still be inside the home.
///
/// The canonicalization is the containment: `File::create` follows symlinks, so
/// without it an agent that can run `ln -s` could point `speech/` outside its
/// own home and have the synthesised clip written there. Mirrors what
/// `workspace::contain_hal_path` does for a writable HAL path.
async fn speech_dir(
    context: &ToolExecutionContext,
    tool_name: &str,
) -> Result<PathBuf, AgentOSError> {
    let home = context.agent_files_dir()?;
    let dir = home.join("speech");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| fail(tool_name, format!("could not create speech directory: {e}")))?;
    let home = home
        .canonicalize()
        .map_err(|e| fail(tool_name, format!("agent home error: {e}")))?;
    let dir = dir
        .canonicalize()
        .map_err(|e| fail(tool_name, format!("could not create speech directory: {e}")))?;
    if !dir.starts_with(&home) {
        return Err(fail(
            tool_name,
            "the speech directory resolves outside the agent home — refusing to write",
        ));
    }
    Ok(dir)
}

/// Drop clips older than [`SPEECH_RETENTION`].
///
/// Best-effort by design: a clip that cannot be removed is not worth failing a
/// synthesis over, and the next call will try again. Bounded by the number of
/// files in one agent's `speech/`.
async fn prune_speech_dir(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let now = std::time::SystemTime::now();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        // Only regular files: never follow a directory or a symlink out.
        if !metadata.is_file() {
            continue;
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > SPEECH_RETENTION);
        if stale {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

#[async_trait]
impl AgentTool for SpeakTool {
    fn name(&self) -> &str {
        "speak"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        // A non-object payload cannot carry `text`, so it fails the same way.
        let fields = payload.as_object().ok_or_else(|| {
            AgentOSError::SchemaValidation(format!("{} requires 'text'", self.name()))
        })?;
        let (text, voice) = validate_speech_request(fields, &self.settings, self.name())?;
        // mp3, not wav: this file is written to be sent to a chat channel,
        // where compactness wins. The host-speaker path (`audio`
        // `action="speak"`) asks for wav, which `pw-play` decodes natively.
        let speech = synthesize_to_file(
            &self.client,
            &self.settings,
            text,
            voice,
            SpeechFormat::Mp3,
            &context,
            self.name(),
        )
        .await?;

        Ok(serde_json::json!({
            "path": speech.rel,
            "bytes": speech.bytes,
            "voice": voice,
            "format": SpeechFormat::Mp3.as_str(),
            "next": "Not played or sent yet. To play it on the host speakers, prefer calling \
                     audio with action=\"speak\" and the text directly — that synthesises and \
                     plays in one call. To send this file to the user call channel-send with \
                     file_path set to this path.",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn ctx(data_dir: &std::path::Path) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
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

    /// One-shot fake `/audio/speech`: drain the JSON request, answer `body`.
    async fn serve_once(content_type: &'static str, body: Vec<u8>) -> String {
        serve_once_capturing(content_type, body).await.0
    }

    /// As `serve_once`, plus the request body the client actually sent — the
    /// only way to assert which `response_format` was asked for.
    async fn serve_once_capturing(
        content_type: &'static str,
        body: Vec<u8>,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (serve_once_inner(content_type, body, Some(tx)).await, rx)
    }

    async fn serve_once_inner(
        content_type: &'static str,
        body: Vec<u8>,
        sent: Option<tokio::sync::oneshot::Sender<String>>,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            // The request is one FLAT JSON object, so its last byte is the only
            // `}`. A test whose text or model name contained a brace, or any
            // nested object in the body, would capture a truncated request or
            // hang here until the client's timeout.
            while !req.ends_with(b"}") {
                let n = sock.read(&mut buf).await.expect("read");
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
            }
            if let Some(sent) = sent {
                let _ = sent.send(String::from_utf8_lossy(&req).into_owned());
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            sock.write_all(head.as_bytes()).await.expect("write head");
            // The client may hang up mid-body once it is over the cap.
            let _ = sock.write_all(&body).await;
        });
        format!("http://{addr}/v1/audio/speech")
    }

    fn enabled(endpoint: String) -> TtsSettings {
        TtsSettings {
            enabled: true,
            endpoint,
            // A variable nothing sets: the no-key path is the local-server path.
            api_key_env: "AGENTOS_TEST_TTS_KEY_UNSET".to_string(),
            ..TtsSettings::default()
        }
    }

    #[tokio::test]
    async fn disabled_says_so_without_a_request() {
        let dir = tempfile::tempdir().unwrap();
        // The default endpoint is a real host; reaching it would be the bug.
        let err = SpeakTool::new(TtsSettings::default())
            .execute(json!({"text": "hello"}), ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not enabled"), "got {err}");
    }

    #[tokio::test]
    async fn bad_input_is_rejected_before_any_request() {
        let dir = tempfile::tempdir().unwrap();
        let tool = SpeakTool::new(enabled("http://127.0.0.1:1/unreachable".into()));
        for payload in [
            json!({}),
            json!({"text": "   "}),
            json!({"text": "x".repeat(MAX_TEXT_CHARS + 1)}),
            json!({"text": "hi", "voice": "../etc"}),
            json!({"text": "hi", "voice": "a\"b"}),
        ] {
            let err = tool
                .execute(payload.clone(), ctx(dir.path()))
                .await
                .unwrap_err();
            assert!(
                matches!(err, AgentOSError::SchemaValidation(_)),
                "{payload} → {err}"
            );
        }
    }

    #[tokio::test]
    async fn audio_lands_in_the_agents_own_speech_dir() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = serve_once("audio/mpeg", b"ID3-fake-mp3".to_vec()).await;
        let context = ctx(dir.path());
        let home = context.agent_files_dir().unwrap();

        let out = SpeakTool::new(enabled(endpoint))
            .execute(json!({"text": "hello there", "voice": ""}), context)
            .await
            .unwrap();

        let rel = out["path"].as_str().unwrap();
        assert!(rel.starts_with("speech/") && rel.ends_with(".mp3"), "{rel}");
        assert_eq!(out["bytes"], 12);
        assert_eq!(out["voice"], "alloy", "empty voice falls back to config");
        assert_eq!(std::fs::read(home.join(rel)).unwrap(), b"ID3-fake-mp3");
    }

    /// The host-speaker path must ask the endpoint for WAV and name the file
    /// to match. Asking for mp3 here costs a failed `pw-play` spawn and a full
    /// ffmpeg transcode before the first sample — and fails outright wherever
    /// ffmpeg is missing.
    #[tokio::test]
    async fn the_wav_format_is_requested_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, sent) = serve_once_capturing("audio/wav", b"RIFF-fake-wav".to_vec()).await;
        let context = ctx(dir.path());
        let home = context.agent_files_dir().unwrap();
        let settings = enabled(endpoint);

        let speech = synthesize_to_file(
            &reqwest::Client::new(),
            &settings,
            "hello there",
            "alloy",
            SpeechFormat::Wav,
            &context,
            "audio",
        )
        .await
        .unwrap();

        assert!(sent.await.unwrap().contains(r#""response_format":"wav""#));
        assert!(speech.rel.starts_with("speech/") && speech.rel.ends_with(".wav"));
        assert_eq!(speech.bytes, 13);
        // The HAL playback driver rejects a relative path, so this must be
        // absolute and must resolve inside the agent's own home.
        // Both sides canonical: the writer canonicalizes for symlink containment.
        assert!(speech.abs.is_absolute());
        assert_eq!(speech.abs, home.join(&speech.rel).canonicalize().unwrap());
        assert_eq!(std::fs::read(&speech.abs).unwrap(), b"RIFF-fake-wav");
    }

    /// `File::create` follows symlinks. An agent that can run `ln -s` must not
    /// be able to point its own `speech/` somewhere else and have the clip
    /// written there.
    #[tokio::test]
    async fn a_symlinked_speech_dir_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let context = ctx(dir.path());
        let home = context.agent_files_dir().unwrap();
        std::os::unix::fs::symlink(outside.path(), home.join("speech")).unwrap();

        // Containment is checked before the request, so an unreachable endpoint
        // is fine: nothing should be synthesised for a home that is bent.
        let err = synthesize_to_file(
            &reqwest::Client::new(),
            &enabled("http://127.0.0.1:1/unreachable".into()),
            "hello",
            "alloy",
            SpeechFormat::Wav,
            &context,
            "speak",
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("outside the agent home"),
            "got {err}"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "a clip was written through the symlink"
        );
    }

    /// The tool name in the error is the tool the operator actually called —
    /// an `audio` failure that claimed to come from `speak` would send anyone
    /// debugging it to the wrong tool.
    #[tokio::test]
    async fn failures_are_attributed_to_the_calling_tool() {
        let dir = tempfile::tempdir().unwrap();
        let err = synthesize_to_file(
            &reqwest::Client::new(),
            &TtsSettings::default(),
            "hello",
            "alloy",
            SpeechFormat::Wav,
            &ctx(dir.path()),
            "audio",
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, AgentOSError::ToolExecutionFailed { tool_name, .. } if tool_name == "audio"),
            "got {err}"
        );
    }

    #[tokio::test]
    async fn a_non_audio_or_oversized_reply_leaves_no_file() {
        for (content_type, body) in [
            ("application/json", br#"{"error":"nope"}"#.to_vec()),
            (
                "audio/mpeg",
                vec![0u8; SpeechFormat::Mp3.max_bytes() as usize + 1],
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let endpoint = serve_once(content_type, body).await;
            let context = ctx(dir.path());
            let home = context.agent_files_dir().unwrap();

            SpeakTool::new(enabled(endpoint))
                .execute(json!({"text": "hello"}), context)
                .await
                .unwrap_err();
            // The body is streamed into a file opened only once the reply is
            // known to be audio, so `speech/` now always exists — but it must
            // hold nothing. Swallowing every `read_dir` error here (rather than
            // only a missing directory) would make this assert nothing at all.
            let leftovers: Vec<_> = match std::fs::read_dir(home.join("speech")) {
                Ok(entries) => entries
                    .map(|entry| entry.expect("read speech/ entry").path())
                    .collect(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(e) => panic!("{content_type}: cannot inspect speech/: {e}"),
            };
            assert!(
                leftovers.is_empty(),
                "{content_type}: file left {leftovers:?}"
            );
        }
    }
}
