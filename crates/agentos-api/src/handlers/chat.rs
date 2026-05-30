//! OpenAI-compatible chat completion endpoint.
//!
//! `POST /api/v1/chat/completions` accepts the standard OpenAI request format
//! and returns either a full response or an SSE stream in OpenAI format.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Extension;
use axum::Json;
use chrono::Utc;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::service::KernelService;
use crate::types::ChatRequest;
use agentos_llm::media::is_supported_image_mime;
use agentos_types::{ContentPart, ImageSource};

// ── OpenAI-format request/response types ────────────────────────────────────

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct OpenAIChatRequest {
    pub model: String,
    pub messages: Vec<OpenAIMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OpenAIMessage {
    pub role: String,
    pub content: OpenAIContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum OpenAIContent {
    Text(String),
    Parts(Vec<OpenAIContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAIContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenAIImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OpenAIImageUrl {
    pub url: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct OpenAIChatResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAIChoice>,
    pub usage: OpenAIUsage,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct OpenAIChoice {
    pub index: u32,
    pub message: OpenAIMessage,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct OpenAIUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Parse `model` field into agent name. Supports:
/// - `"agent-name"` — uses the agent's default model
/// - `"provider/model"` — uses the provider portion as agent name hint
fn parse_model(model: &str) -> String {
    if let Some((agent, _model)) = model.split_once('/') {
        agent.to_string()
    } else {
        model.to_string()
    }
}

fn openai_content_plain_text(content: &OpenAIContent) -> String {
    match content {
        OpenAIContent::Text(s) => s.clone(),
        OpenAIContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                OpenAIContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn parse_data_uri_image(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| "invalid data URI".to_string())?;
    let (meta, b64) = rest
        .split_once(',')
        .ok_or_else(|| "invalid data URI (no comma)".to_string())?;
    let mime = meta
        .split(';')
        .next()
        .unwrap_or("image/png")
        .trim()
        .to_ascii_lowercase();
    if !is_supported_image_mime(&mime) {
        return Err(format!("unsupported image MIME in data URI: {mime}"));
    }
    Ok((mime, b64.trim().to_string()))
}

fn openai_image_url_to_part(img: &OpenAIImageUrl) -> Result<ContentPart, String> {
    let url = img.url.trim();
    let lc = url.to_ascii_lowercase();
    if lc.starts_with("data:image/") {
        let (mime, data) = parse_data_uri_image(url)?;
        return Ok(ContentPart::Image {
            mime,
            source: ImageSource::Base64 { data },
        });
    }
    if lc.starts_with("https://") || lc.starts_with("http://") {
        return Ok(ContentPart::Image {
            mime: "image/jpeg".to_string(),
            source: ImageSource::Url {
                url: url.to_string(),
            },
        });
    }
    Err(format!(
        "unsupported image_url scheme (allowed: data:image/...;base64,... or http(s)://): {}",
        &url[..url.len().min(64)]
    ))
}

fn openai_content_to_parts(content: &OpenAIContent) -> Result<Vec<ContentPart>, String> {
    match content {
        OpenAIContent::Text(s) => Ok(vec![ContentPart::Text { text: s.clone() }]),
        OpenAIContent::Parts(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p {
                    OpenAIContentPart::Text { text } => {
                        if !text.is_empty() {
                            out.push(ContentPart::Text { text: text.clone() });
                        }
                    }
                    OpenAIContentPart::ImageUrl { image_url } => {
                        out.push(openai_image_url_to_part(image_url)?);
                    }
                }
            }
            if out.is_empty() {
                return Err("message content produced no parts".into());
            }
            Ok(out)
        }
    }
}

fn parts_concat_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parts_contain_image(parts: &[ContentPart]) -> bool {
    parts.iter().any(|p| matches!(p, ContentPart::Image { .. }))
}

fn estimate_openai_prompt_tokens(messages: &[OpenAIMessage]) -> u32 {
    let mut n: u32 = 0;
    let mut imgs: u32 = 0;
    for m in messages {
        match &m.content {
            OpenAIContent::Text(s) => n = n.saturating_add((s.len() as u32).saturating_div(4)),
            OpenAIContent::Parts(parts) => {
                for p in parts {
                    match p {
                        OpenAIContentPart::Text { text } => {
                            n = n.saturating_add((text.len() as u32).saturating_div(4));
                        }
                        OpenAIContentPart::ImageUrl { .. } => {
                            imgs = imgs.saturating_add(1);
                        }
                    }
                }
            }
        }
    }
    n.saturating_add(imgs.saturating_mul(1500))
}

/// Convert OpenAI messages to history pairs, plain-text user line, and typed parts for the last user turn.
#[allow(clippy::type_complexity)]
fn messages_to_history(
    messages: &[OpenAIMessage],
) -> Result<(Vec<(String, String)>, String, Vec<ContentPart>), String> {
    for msg in messages {
        if msg.role != "user" && msg.role != "assistant" {
            return Err(format!(
                "role '{}' is not supported; only 'user' and 'assistant' are accepted",
                msg.role
            ));
        }
    }

    for window in messages.windows(2) {
        if window[0].role == window[1].role {
            return Err(format!(
                "consecutive '{}' messages are not valid; messages must alternate roles",
                window[0].role
            ));
        }
    }

    let Some(last_user_idx) = messages.iter().rposition(|m| m.role == "user") else {
        return Err("No user message found in messages array".into());
    };

    let mut history = Vec::new();
    let mut i = 0usize;
    while i < messages.len() {
        let msg = &messages[i];
        if msg.role == "user" {
            if i == last_user_idx {
                i += 1;
                continue;
            }
            if i + 1 < messages.len() && messages[i + 1].role == "assistant" {
                history.push((
                    openai_content_plain_text(&msg.content),
                    openai_content_plain_text(&messages[i + 1].content),
                ));
                i += 2;
                continue;
            }
            return Err(
                "Each user message except the last must be followed by an assistant message".into(),
            );
        }
        i += 1;
    }

    let last = &messages[last_user_idx];
    let parts = openai_content_to_parts(&last.content)?;
    let user_plain = parts_concat_text(&parts);
    let has_image = parts_contain_image(&parts);
    if user_plain.trim().is_empty() && !has_image {
        return Err("No user text or image content in the final user message".into());
    }

    Ok((history, user_plain, parts))
}

fn generate_id() -> String {
    format!(
        "chatcmpl-{}",
        &uuid::Uuid::new_v4().to_string().replace('-', "")[..24]
    )
}

fn openai_vision_not_supported_response() -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": {
                "message": "selected model does not support image input",
                "type": "invalid_request_error",
            }
        })),
    )
        .into_response()
}

fn openai_request_to_chat(req: &OpenAIChatRequest) -> Result<ChatRequest, String> {
    let agent_name = parse_model(&req.model);
    let (history, user_message, parts) = messages_to_history(&req.messages)?;
    Ok(ChatRequest {
        session_id: String::new(),
        agent_name,
        message: user_message,
        history,
        parts,
    })
}

// ── Handler ─────────────────────────────────────────────────────────────────

/// `POST /api/v1/chat/completions` — OpenAI-compatible chat completion.
///
/// NOTE: This endpoint follows the OpenAI response format for client library
/// compatibility and does NOT use the standard `{ "data": ... }` envelope.
///
/// When `stream: true`, returns an SSE stream of OpenAI-format chunks.
#[utoipa::path(
    post,
    path = "/api/v1/chat/completions",
    tag = "chat",
    operation_id = "chat_completions",
    request_body = OpenAIChatRequest,
    responses(
        (status = 200, description = "OpenAI-compatible chat completion. When request `stream` is true, responds with text/event-stream SSE chunks instead of this JSON body.", body = OpenAIChatResponse),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn completions(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<OpenAIChatRequest>,
) -> Result<axum::response::Response, ApiError> {
    require_permission(&key, "chat:w")?;

    let chat_req = openai_request_to_chat(&req).map_err(ApiError::BadRequest)?;

    if parts_contain_image(&chat_req.parts) {
        let ok = svc.agent_supports_images(&chat_req.agent_name).await?;
        if !ok {
            return Ok(openai_vision_not_supported_response());
        }
    }

    if req.stream {
        return stream_completions(svc, chat_req, req.model).await;
    }

    let response = svc.chat_send(chat_req).await?;

    let prompt_tokens = estimate_openai_prompt_tokens(&req.messages);
    let completion_tokens = (response.message.len() / 4) as u32;

    let reply = OpenAIChatResponse {
        id: generate_id(),
        object: "chat.completion".to_string(),
        created: Utc::now().timestamp(),
        model: req.model,
        choices: vec![OpenAIChoice {
            index: 0,
            message: OpenAIMessage {
                role: "assistant".to_string(),
                content: OpenAIContent::Text(response.message),
            },
            finish_reason: "stop".to_string(),
        }],
        usage: OpenAIUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    };

    Ok(Json(reply).into_response())
}

/// Streaming variant: runs inference via `chat_stream` and maps
/// `ChatStreamEvent`s to OpenAI-format SSE chunks.
async fn stream_completions(
    svc: Arc<dyn KernelService>,
    chat_req: ChatRequest,
    model: String,
) -> Result<axum::response::Response, ApiError> {
    let (tx, rx) = tokio::sync::mpsc::channel::<agentos_kernel::ChatStreamEvent>(32);
    let id = generate_id();
    let created = Utc::now().timestamp();

    tokio::spawn(async move {
        if let Err(e) = svc.chat_stream(chat_req, tx.clone()).await {
            let _ = tx
                .send(agentos_kernel::ChatStreamEvent::Error {
                    message: e.to_string(),
                })
                .await;
        }
    });

    let stream = ReceiverStream::new(rx)
        .map(move |event| {
            let chunk = match event {
                agentos_kernel::ChatStreamEvent::Thinking { .. } => {
                    serde_json::json!({
                        "id": &id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": &model,
                        "choices": [{
                            "index": 0,
                            "delta": {"role": "assistant"},
                            "finish_reason": serde_json::Value::Null
                        }]
                    })
                }
                agentos_kernel::ChatStreamEvent::TextChunk { text } => {
                    serde_json::json!({
                        "id": &id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": &model,
                        "choices": [{
                            "index": 0,
                            "delta": {"content": text},
                            "finish_reason": serde_json::Value::Null
                        }]
                    })
                }
                agentos_kernel::ChatStreamEvent::ToolStart { tool_name, .. } => {
                    serde_json::json!({
                        "id": &id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": &model,
                        "choices": [{
                            "index": 0,
                            "delta": {"content": format!("\n[calling tool: {tool_name}]\n")},
                            "finish_reason": serde_json::Value::Null
                        }]
                    })
                }
                agentos_kernel::ChatStreamEvent::ToolResult {
                    tool_name,
                    result_preview,
                    ..
                } => {
                    serde_json::json!({
                        "id": &id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": &model,
                        "choices": [{
                            "index": 0,
                            "delta": {"content": format!("[{tool_name} result: {result_preview}]\n")},
                            "finish_reason": serde_json::Value::Null
                        }]
                    })
                }
                agentos_kernel::ChatStreamEvent::Done { .. } => {
                    serde_json::json!({
                        "id": &id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": &model,
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": "stop"
                        }]
                    })
                }
                agentos_kernel::ChatStreamEvent::Error { message } => {
                    serde_json::json!({
                        "error": {"message": message, "type": "server_error"}
                    })
                }
            };

            let data = serde_json::to_string(&chunk).unwrap_or_default();
            Event::default().data(data)
        })
        .chain(futures::stream::once(async {
            Event::default().data("[DONE]")
        }))
        .map(Ok::<_, Infallible>);

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

#[cfg(test)]
mod multimodal_chat_tests {
    use super::*;
    #[test]
    fn accepts_string_content_maps_to_parts() {
        let msgs = vec![OpenAIMessage {
            role: "user".to_string(),
            content: OpenAIContent::Text("hi".to_string()),
        }];
        let (history, plain, parts) = messages_to_history(&msgs).expect("history");
        assert!(history.is_empty());
        assert_eq!(plain, "hi");
        assert_eq!(parts.len(), 1);
        assert_eq!(
            parts[0],
            ContentPart::Text {
                text: "hi".to_string()
            }
        );
    }

    #[test]
    fn accepts_parts_array_text_only() {
        let msgs = vec![OpenAIMessage {
            role: "user".to_string(),
            content: OpenAIContent::Parts(vec![OpenAIContentPart::Text {
                text: "hello".into(),
            }]),
        }];
        let (history, plain, _) = messages_to_history(&msgs).unwrap();
        assert!(history.is_empty());
        assert_eq!(plain, "hello");
    }

    #[test]
    fn accepts_data_uri_image_part() {
        let url =
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let msgs = vec![OpenAIMessage {
            role: "user".to_string(),
            content: OpenAIContent::Parts(vec![OpenAIContentPart::ImageUrl {
                image_url: OpenAIImageUrl {
                    url: url.into(),
                    detail: None,
                },
            }]),
        }];
        let (_, _, parts) = messages_to_history(&msgs).unwrap();
        assert!(parts.iter().any(|p| matches!(p, ContentPart::Image { .. })));
    }

    #[test]
    fn accepts_https_image_url_placeholder_mime() {
        let msgs = vec![OpenAIMessage {
            role: "user".to_string(),
            content: OpenAIContent::Parts(vec![OpenAIContentPart::ImageUrl {
                image_url: OpenAIImageUrl {
                    url: "https://example.com/cat.png".to_string(),
                    detail: Some("auto".into()),
                },
            }]),
        }];
        let (_, _, parts) = messages_to_history(&msgs).unwrap();
        let p = &parts[0];
        let ContentPart::Image { mime, source } = p else {
            panic!("expected image part");
        };
        assert_eq!(mime.as_str(), "image/jpeg");
        match source {
            ImageSource::Url { url } => assert!(url.ends_with("/cat.png")),
            _ => panic!("expected url source"),
        }
    }

    #[test]
    fn rejects_unknown_url_scheme_for_image() {
        let msgs = vec![OpenAIMessage {
            role: "user".to_string(),
            content: OpenAIContent::Parts(vec![OpenAIContentPart::ImageUrl {
                image_url: OpenAIImageUrl {
                    url: "file:///etc/passwd".to_string(),
                    detail: None,
                },
            }]),
        }];
        assert!(messages_to_history(&msgs).is_err());
    }

    #[test]
    fn consecutive_roles_still_invalid_with_multimodal_parts() {
        let msgs = vec![
            OpenAIMessage {
                role: "user".to_string(),
                content: OpenAIContent::Text("a".into()),
            },
            OpenAIMessage {
                role: "user".to_string(),
                content: OpenAIContent::Text("b".into()),
            },
        ];
        assert!(messages_to_history(&msgs).is_err());
    }

    #[test]
    fn openai_estimate_counts_image_as_extra_tokens() {
        let msgs = vec![OpenAIMessage {
            role: "user".to_string(),
            content: OpenAIContent::Parts(vec![
                OpenAIContentPart::Text { text: "x".into() },
                OpenAIContentPart::ImageUrl {
                    image_url: OpenAIImageUrl {
                        url: "https://example.com/i.png".into(),
                        detail: None,
                    },
                },
            ]),
        }];
        let n = estimate_openai_prompt_tokens(&msgs);
        assert!(n >= 1500);
    }
}
