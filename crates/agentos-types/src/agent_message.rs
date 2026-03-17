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

    /// Canonical bytes to sign: stable JSON encoding of id, from, to, content, and timestamp.
    /// Uses serde_json for deterministic serialization (not Debug, which is unstable across
    /// compiler versions).
    ///
    /// # Panics
    /// Never in practice — `MessageTarget` and `MessageContent` both derive `Serialize`
    /// with no infallible paths. A panic here would indicate a bug in the type definitions.
    pub fn signing_payload(&self) -> Vec<u8> {
        let canonical = serde_json::json!({
            "id": self.id.to_string(),
            "from": self.from.to_string(),
            "to": serde_json::to_value(&self.to)
                .expect("MessageTarget serialization is infallible"),
            "content": serde_json::to_value(&self.content)
                .expect("MessageContent serialization is infallible"),
            "timestamp": self.timestamp.timestamp(),
        });
        canonical.to_string().into_bytes()
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
