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
