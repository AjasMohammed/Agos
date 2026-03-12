use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: MessageID,
    pub from: AgentID,
    pub to: MessageTarget,
    pub content: MessageContent,
    pub reply_to: Option<MessageID>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub trace_id: TraceID,
    /// Ed25519 signature (hex) over canonical message fields.
    /// None for kernel-generated messages without a sender identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Message TTL in seconds. Receiving side MUST reject expired messages.
    #[serde(default = "default_ttl")]
    pub ttl_seconds: u64,
    /// Absolute expiry (timestamp + ttl_seconds). Set by sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn default_ttl() -> u64 {
    60
}

impl AgentMessage {
    /// Returns true if the message has expired.
    pub fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            chrono::Utc::now() > exp
        } else {
            false
        }
    }

    /// Canonical bytes to sign: id|from|to_str|content_hash|timestamp.
    pub fn signing_payload(&self) -> Vec<u8> {
        let to_str = format!("{:?}", self.to);
        let content_str = format!("{:?}", self.content);
        format!(
            "{}|{}|{}|{}|{}",
            self.id,
            self.from,
            to_str,
            content_str,
            self.timestamp.timestamp(),
        )
        .into_bytes()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageTarget {
    Direct(AgentID),
    DirectByName(String),
    Group(GroupID),
    Broadcast,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageContent {
    Text(String),
    Structured(serde_json::Value),
    TaskDelegation {
        prompt: String,
        priority: u8,
        timeout_secs: u64,
    },
    TaskResult {
        task_id: TaskID,
        result: serde_json::Value,
    },
}
