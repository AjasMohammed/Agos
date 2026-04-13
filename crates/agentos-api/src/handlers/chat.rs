//! OpenAI-compatible chat completion endpoint.
//!
//! `POST /api/v1/chat/completions` accepts the standard OpenAI request format
//! and returns either a full response or an SSE stream in OpenAI format.

use axum::extract::State;
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

// ── OpenAI-format request/response types ────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAIChatResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAIChoice>,
    pub usage: OpenAIUsage,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAIChoice {
    pub index: u32,
    pub message: OpenAIMessage,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Serialize)]
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

/// Convert OpenAI messages to history pairs suitable for `ChatRequest`.
///
/// Returns `Err` if any message has an unrecognised role (only `user` and
/// `assistant` are accepted — `system` and `tool` are not yet supported and
/// must be rejected rather than silently dropped), or if two consecutive
/// messages share the same role.
fn messages_to_history(
    messages: &[OpenAIMessage],
) -> Result<(Vec<(String, String)>, String), String> {
    // Reject unrecognised roles. System prompts and tool messages are not yet
    // supported by the underlying ChatRequest; return an error rather than
    // silently dropping them.
    for msg in messages {
        if msg.role != "user" && msg.role != "assistant" {
            return Err(format!(
                "role '{}' is not supported; only 'user' and 'assistant' are accepted",
                msg.role
            ));
        }
    }

    // Reject consecutive same-role messages — they are structurally invalid and
    // would silently drop content if we tried to pair them up.
    for window in messages.windows(2) {
        if window[0].role == window[1].role {
            return Err(format!(
                "consecutive '{}' messages are not valid; messages must alternate roles",
                window[0].role
            ));
        }
    }

    let mut history = Vec::new();
    let mut last_user_msg = String::new();

    // Collect pairs: (user, assistant) and extract the final user message.
    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];
        if msg.role == "user" {
            if i + 1 < messages.len() && messages[i + 1].role == "assistant" {
                history.push((msg.content.clone(), messages[i + 1].content.clone()));
                i += 2;
                continue;
            } else {
                // Final user message (no assistant reply yet)
                last_user_msg = msg.content.clone();
            }
        }
        i += 1;
    }

    Ok((history, last_user_msg))
}

fn generate_id() -> String {
    format!(
        "chatcmpl-{}",
        &uuid::Uuid::new_v4().to_string().replace('-', "")[..24]
    )
}

// ── Handler ─────────────────────────────────────────────────────────────────

/// `POST /api/v1/chat/completions` — OpenAI-compatible chat completion.
///
/// NOTE: This endpoint follows the OpenAI response format for client library
/// compatibility and does NOT use the standard `{ "data": ... }` envelope.
///
/// When `stream: true`, returns an SSE stream of OpenAI-format chunks.
pub async fn completions(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<OpenAIChatRequest>,
) -> Result<axum::response::Response, ApiError> {
    require_permission(&key, "chat:w")?;

    let agent_name = parse_model(&req.model);
    let (history, user_message) =
        messages_to_history(&req.messages).map_err(ApiError::BadRequest)?;

    if user_message.is_empty() {
        return Err(ApiError::BadRequest(
            "No user message found in messages array".into(),
        ));
    }

    let chat_req = ChatRequest {
        session_id: String::new(),
        agent_name: agent_name.clone(),
        message: user_message,
        history,
    };

    if req.stream {
        return stream_completions(svc, chat_req, req.model).await;
    }

    let response = svc.chat_send(chat_req).await?;

    let prompt_tokens = req
        .messages
        .iter()
        .map(|m| m.content.len() / 4) // rough estimate
        .sum::<usize>() as u32;
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
                content: response.message,
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

    // Spawn inference in the background so the SSE stream starts immediately.
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
                    // Emit an empty delta to signal the stream has started.
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
                    // Text was already streamed via TextChunk events;
                    // send a final chunk with finish_reason to close the stream.
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
        // Append the OpenAI-spec `[DONE]` sentinel after the channel closes.
        .chain(futures::stream::once(async {
            Event::default().data("[DONE]")
        }))
        .map(Ok::<_, Infallible>);

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}
