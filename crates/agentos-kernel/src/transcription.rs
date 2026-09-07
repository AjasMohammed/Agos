//! Speech-to-text for inbound channel voice/audio messages.
//!
//! Posts audio bytes to an OpenAI-compatible `/audio/transcriptions` endpoint
//! (multipart `file` + `model`) using a Bearer key read from the environment.
//! Used by the `InboundRouter` to turn a Telegram voice note into text the
//! agent can read — the Hermes-style "transcribe, don't drop" path.

pub use crate::config::TranscriptionSettings;

/// Transcribe `audio` to text via the configured OpenAI-compatible endpoint.
///
/// `filename` is sent as the multipart part name (the extension hints the
/// server at the codec, e.g. `voice.ogg`). Returns the transcript text, or an
/// error string (caller logs and falls back to a media note). The API key is
/// read from the env var named by `settings.api_key_env`; nothing is logged.
pub async fn transcribe_audio(
    client: &reqwest::Client,
    settings: &TranscriptionSettings,
    audio: Vec<u8>,
    filename: &str,
) -> Result<String, String> {
    let api_key = std::env::var(&settings.api_key_env).map_err(|_| {
        format!(
            "transcription API key env '{}' not set",
            settings.api_key_env
        )
    })?;
    if api_key.trim().is_empty() {
        return Err(format!(
            "transcription API key env '{}' is empty",
            settings.api_key_env
        ));
    }

    let part = reqwest::multipart::Part::bytes(audio)
        .file_name(filename.to_string())
        .mime_str("application/octet-stream")
        .map_err(|e| format!("multipart build: {e}"))?;
    let form = reqwest::multipart::Form::new()
        .text("model", settings.model.clone())
        .part("file", part);

    let resp = client
        .post(&settings.endpoint)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|_| "transcription request failed (details redacted)".to_string())?;

    if !resp.status().is_success() {
        return Err(format!("transcription HTTP {}", resp.status().as_u16()));
    }

    // OpenAI returns `{ "text": "..." }`.
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| "transcription response parse failed".to_string())?;
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("transcription returned empty text".to_string());
    }
    Ok(text)
}
