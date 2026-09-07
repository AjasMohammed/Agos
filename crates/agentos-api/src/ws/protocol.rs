//! WebSocket protocol types — client-to-server and server-to-client frames.
//!
//! All frames are JSON-encoded with a `type` discriminator field.

use serde::{Deserialize, Serialize};

/// Client → Server frames (JSON).
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientFrame {
    /// Subscribe to a named event channel (e.g. "tasks", "agents", "audit").
    #[serde(rename = "subscribe")]
    Subscribe {
        channel: String,
        #[serde(default)]
        filter: serde_json::Value,
    },

    /// Unsubscribe from a previously subscribed channel.
    #[serde(rename = "unsubscribe")]
    Unsubscribe { subscription_id: String },

    /// Send a chat message to an agent.
    #[serde(rename = "chat.send")]
    ChatSend {
        session_id: String,
        message: String,
        #[serde(default)]
        agent_name: String,
    },

    /// Respond to a notification.
    #[serde(rename = "notification.respond")]
    NotificationRespond { id: String, text: String },

    /// Cancel an in-progress chat response.
    #[serde(rename = "chat.cancel")]
    ChatCancel { session_id: String },

    /// Cancel a running task.
    #[serde(rename = "task.cancel")]
    TaskCancel { task_id: String },

    /// Heartbeat ping from client.
    #[serde(rename = "ping")]
    Ping,
}

/// Server → Client frames (JSON).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerFrame {
    /// Confirms a successful channel subscription.
    #[serde(rename = "subscribed")]
    Subscribed {
        channel: String,
        subscription_id: String,
    },

    /// Confirms a channel was unsubscribed.
    #[serde(rename = "unsubscribed")]
    Unsubscribed { subscription_id: String },

    /// A real-time event pushed from a subscribed channel.
    #[serde(rename = "event")]
    Event {
        channel: String,
        event: String,
        data: serde_json::Value,
    },

    /// A streamed chat response chunk.
    #[serde(rename = "chat.chunk")]
    ChatChunk { session_id: String, delta: String },

    /// Chat response completed.
    #[serde(rename = "chat.done")]
    ChatDone {
        session_id: String,
        tool_calls: Vec<serde_json::Value>,
    },

    /// Chat was cancelled by the client.
    #[serde(rename = "chat.cancelled")]
    ChatCancelled { session_id: String },

    /// Protocol or processing error.
    #[serde(rename = "error")]
    Error { code: String, message: String },

    /// Heartbeat pong response.
    #[serde(rename = "pong")]
    Pong,
}

/// Wire tags of every [`ClientFrame`] variant, in declaration order. Consumed
/// by the `gen-events` bin so the panel's protocol mirror is generated rather
/// than hand-maintained.
///
/// Drift guard: adding an enum variant fails compilation in `tag_tests`'
/// exhaustive matches, which forces a same-file edit next to these slices;
/// the round-trip tests then pin slice ↔ serde agreement. The slice itself is
/// still hand-listed — update it in the same edit as the match arm.
// ponytail: full enum→slice pinning needs strum::EnumCount; add strum if this
// ever drifts in practice.
pub const CLIENT_FRAME_TAGS: &[&str] = &[
    "subscribe",
    "unsubscribe",
    "chat.send",
    "notification.respond",
    "chat.cancel",
    "task.cancel",
    "ping",
];

/// Wire tags of every [`ServerFrame`] variant, in declaration order.
pub const SERVER_FRAME_TAGS: &[&str] = &[
    "subscribed",
    "unsubscribed",
    "event",
    "chat.chunk",
    "chat.done",
    "chat.cancelled",
    "error",
    "pong",
];

#[cfg(test)]
mod tag_tests {
    use super::*;

    // Exhaustive matches: adding an enum variant breaks compilation HERE.
    // WHEN YOU ADD AN ARM: add the tag to CLIENT_FRAME_TAGS/SERVER_FRAME_TAGS
    // above, add a sample below, and regenerate the panel mirror
    // (cargo run -p agentos-api --bin gen-events, then panel sync).
    fn client_tag(f: &ClientFrame) -> &'static str {
        match f {
            ClientFrame::Subscribe { .. } => "subscribe",
            ClientFrame::Unsubscribe { .. } => "unsubscribe",
            ClientFrame::ChatSend { .. } => "chat.send",
            ClientFrame::NotificationRespond { .. } => "notification.respond",
            ClientFrame::ChatCancel { .. } => "chat.cancel",
            ClientFrame::TaskCancel { .. } => "task.cancel",
            ClientFrame::Ping => "ping",
        }
    }

    fn server_tag(f: &ServerFrame) -> &'static str {
        match f {
            ServerFrame::Subscribed { .. } => "subscribed",
            ServerFrame::Unsubscribed { .. } => "unsubscribed",
            ServerFrame::Event { .. } => "event",
            ServerFrame::ChatChunk { .. } => "chat.chunk",
            ServerFrame::ChatDone { .. } => "chat.done",
            ServerFrame::ChatCancelled { .. } => "chat.cancelled",
            ServerFrame::Error { .. } => "error",
            ServerFrame::Pong => "pong",
        }
    }

    /// Every CLIENT_FRAME_TAGS entry deserializes to the variant whose
    /// exhaustive-match tag equals the entry — TAGS ↔ serde ↔ enum all agree.
    #[test]
    fn client_tags_round_trip_through_serde() {
        for tag in CLIENT_FRAME_TAGS {
            // Minimal required fields per variant.
            let payload = match *tag {
                "subscribe" => r#"{"type":"subscribe","channel":"tasks"}"#.to_string(),
                "unsubscribe" => r#"{"type":"unsubscribe","subscription_id":"s"}"#.to_string(),
                "chat.send" => r#"{"type":"chat.send","session_id":"s","message":"m"}"#.to_string(),
                "notification.respond" => {
                    r#"{"type":"notification.respond","id":"n","text":"t"}"#.to_string()
                }
                "chat.cancel" => r#"{"type":"chat.cancel","session_id":"s"}"#.to_string(),
                "task.cancel" => r#"{"type":"task.cancel","task_id":"t"}"#.to_string(),
                other => format!(r#"{{"type":"{other}"}}"#),
            };
            let frame: ClientFrame = serde_json::from_str(&payload)
                .unwrap_or_else(|e| panic!("tag {tag} does not deserialize: {e}"));
            assert_eq!(client_tag(&frame), *tag);
        }
        assert_eq!(
            CLIENT_FRAME_TAGS.len(),
            7,
            "update TAGS with the new variant"
        );
    }

    /// Every ServerFrame variant serializes to a `type` field present in
    /// SERVER_FRAME_TAGS, and the slice has no extras.
    #[test]
    fn server_tags_round_trip_through_serde() {
        let samples = [
            ServerFrame::Subscribed {
                channel: "tasks".into(),
                subscription_id: "s".into(),
            },
            ServerFrame::Unsubscribed {
                subscription_id: "s".into(),
            },
            ServerFrame::Event {
                channel: "tasks".into(),
                event: "task.completed".into(),
                data: serde_json::Value::Null,
            },
            ServerFrame::ChatChunk {
                session_id: "s".into(),
                delta: "d".into(),
            },
            ServerFrame::ChatDone {
                session_id: "s".into(),
                tool_calls: vec![],
            },
            ServerFrame::ChatCancelled {
                session_id: "s".into(),
            },
            ServerFrame::Error {
                code: "E".into(),
                message: "m".into(),
            },
            ServerFrame::Pong,
        ];
        assert_eq!(samples.len(), SERVER_FRAME_TAGS.len());
        for (frame, tag) in samples.iter().zip(SERVER_FRAME_TAGS) {
            let v = serde_json::to_value(frame).unwrap();
            assert_eq!(v["type"], *tag);
            assert_eq!(server_tag(frame), *tag);
        }
    }
}
