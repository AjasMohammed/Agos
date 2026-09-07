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
    /// Plain-text projection for previews/logging. Omits media URLs — use
    /// [`render_for_delivery`](Self::render_for_delivery) when actually sending.
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

    /// Render content for delivery on channels without native media support,
    /// INCLUDING any media URL so the link is actually delivered (and
    /// auto-embeds on platforms like Slack/Discord). Adapters with native media
    /// APIs may special-case `Image`/`File` instead of calling this.
    pub fn render_for_delivery(&self) -> String {
        match self {
            MessageContent::Text(s) | MessageContent::Markdown(s) => s.clone(),
            MessageContent::Image { url, alt } => match alt {
                Some(a) if !a.trim().is_empty() => format!("{a}\n{url}"),
                _ => url.clone(),
            },
            MessageContent::File { url, filename, .. } => {
                if filename.trim().is_empty() {
                    url.clone()
                } else {
                    format!("{filename}: {url}")
                }
            }
            MessageContent::Mixed(parts) => parts
                .iter()
                .map(|p| p.render_for_delivery())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// All image URLs in this content (recursing `Mixed`). Used by adapters
    /// with native image messages (LINE/WhatsApp/Teams).
    pub fn image_urls(&self) -> Vec<String> {
        match self {
            MessageContent::Image { url, .. } => vec![url.clone()],
            MessageContent::Mixed(parts) => parts.iter().flat_map(|p| p.image_urls()).collect(),
            _ => Vec::new(),
        }
    }

    /// All file (non-image) attachments as `(url, filename, mime)`.
    pub fn files(&self) -> Vec<(String, String, String)> {
        match self {
            MessageContent::File {
                url,
                filename,
                mime,
            } => {
                vec![(url.clone(), filename.clone(), mime.clone())]
            }
            MessageContent::Mixed(parts) => parts.iter().flat_map(|p| p.files()).collect(),
            _ => Vec::new(),
        }
    }

    /// The textual caption only (Text/Markdown parts + image alt text), with no
    /// media URLs — for use alongside a native image/file message.
    pub fn text_caption(&self) -> String {
        match self {
            MessageContent::Text(s) | MessageContent::Markdown(s) => s.clone(),
            MessageContent::Image { alt, .. } => alt.clone().unwrap_or_default(),
            MessageContent::File { .. } => String::new(),
            MessageContent::Mixed(parts) => parts
                .iter()
                .map(|p| p.text_caption())
                .filter(|s| !s.trim().is_empty())
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
