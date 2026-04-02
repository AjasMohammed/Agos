//! OpenAI-compatible chat completion endpoint.
//!
//! `POST /api/v1/chat/completions` accepts the standard OpenAI request format
//! and returns either a full response or SSE stream in OpenAI chunk format.

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Extension;
use axum::Json;
use chrono::Utc;
use futures::stream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_stream::StreamExt;

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

#[derive(Debug, Clone, Serialize)]
pub struct OpenAIChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAIChunkChoice>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAIChunkChoice {
    pub index: u32,
    pub delta: OpenAIChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAIChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Parse `model` field into (agent_name, optional_model). Supports:
/// - `"agent-name"` — uses the agent's default model
/// - `"provider/model"` — uses the first part as agent hint
fn parse_model(model: &str) -> String {
    // If model contains "/" it's a "provider/model" string; use provider
    // portion as agent name hint. Otherwise use it directly.
    if let Some((agent, _model)) = model.split_once('/') {
        agent.to_string()
    } else {
        model.to_string()
    }
}

/// Convert OpenAI messages to history pairs suitable for `ChatRequest`.
///
/// Returns `Err` if two consecutive messages share the same role, which is
/// not a valid conversation structure and typically indicates a client bug.
fn messages_to_history(
    messages: &[OpenAIMessage],
) -> Result<(Vec<(String, String)>, String), String> {
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
            // Check if next message is assistant
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
/// NOTE: This endpoint intentionally follows the OpenAI response format for
/// client library compatibility, so it does NOT use the standard `{ "data": ... }`
/// envelope that other endpoints use.
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
        // Streaming response via SSE.
        let response = svc.chat_send(chat_req).await?;
        let id = generate_id();
        let created = Utc::now().timestamp();
        let model = req.model.clone();

        // Simulate streaming by chunking the response into words.
        let words: Vec<String> = response
            .message
            .split_inclusive(' ')
            .map(|s| s.to_string())
            .collect();

        let initial_chunk = OpenAIChunk {
            id: id.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model.clone(),
            choices: vec![OpenAIChunkChoice {
                index: 0,
                delta: OpenAIChunkDelta {
                    role: Some("assistant".to_string()),
                    content: None,
                },
                finish_reason: None,
            }],
        };

        let id_clone = id.clone();
        let model_clone = model.clone();

        let word_chunks = words.into_iter().map(move |word| OpenAIChunk {
            id: id_clone.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model_clone.clone(),
            choices: vec![OpenAIChunkChoice {
                index: 0,
                delta: OpenAIChunkDelta {
                    role: None,
                    content: Some(word),
                },
                finish_reason: None,
            }],
        });

        let done_chunk = OpenAIChunk {
            id,
            object: "chat.completion.chunk".to_string(),
            created,
            model,
            choices: vec![OpenAIChunkChoice {
                index: 0,
                delta: OpenAIChunkDelta {
                    role: None,
                    content: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
        };

        let chunks = std::iter::once(initial_chunk)
            .chain(word_chunks)
            .chain(std::iter::once(done_chunk));

        let event_stream = stream::iter(chunks).map(|chunk| {
            let data =
                serde_json::to_string(&chunk).expect("OpenAIChunk serialization is infallible");
            Ok::<_, std::convert::Infallible>(Event::default().data(data))
        });

        // Append a final `[DONE]` event.
        let done_stream = stream::once(async { Ok(Event::default().data("[DONE]")) });

        let combined = event_stream.chain(done_stream);

        Ok(Sse::new(combined)
            .keep_alive(KeepAlive::default())
            .into_response())
    } else {
        // Non-streaming response.
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
}
