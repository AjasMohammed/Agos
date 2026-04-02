use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type MessageID = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: MessageID,
    pub channel_type: String,
    pub sender: ChannelIdentity,
    pub content: MessageContent,
    pub thread_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelIdentity {
    pub platform_id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum MessageContent {
    Text(String),
    Markdown(String),
    Image {
        url: String,
        alt: Option<String>,
    },
    File {
        url: String,
        filename: String,
        mime: String,
    },
    Mixed(Vec<MessageContent>),
}

impl MessageContent {
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) | MessageContent::Markdown(s) => s.clone(),
            MessageContent::Image { alt, .. } => alt.clone().unwrap_or_default(),
            MessageContent::File { filename, .. } => format!("[file: {}]", filename),
            MessageContent::Mixed(parts) => parts
                .iter()
                .map(|p| p.as_text())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChannelCapabilities {
    pub threads: bool,
    pub reactions: bool,
    pub media: bool,
    pub rich_formatting: bool,
    pub max_message_length: usize,
}

#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub channel_instance_id: String,
    pub content: MessageContent,
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeliveryReceipt {
    pub message_id: MessageID,
    pub delivered_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub id: MessageID,
    pub channel_type: String,
    pub channel_instance_id: String,
    pub sender: ChannelIdentity,
    pub content: MessageContent,
    pub thread_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub raw: serde_json::Value,
}
