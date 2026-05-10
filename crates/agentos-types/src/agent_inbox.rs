use crate::{AgentID, AgentInboxEntryID, AgentMessageEntryID};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Category of an agent-facing async notification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentInboxKind {
    /// A scheduled once-job or recurring task completed.
    Scheduled,
    /// An event the agent subscribed to fired.
    Event,
    /// A sub-agent spawned via `spawn_async` finished.
    AsyncDone,
    /// A timer set by the agent fired.
    Timer,
}

impl AgentInboxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Event => "event",
            Self::AsyncDone => "async_done",
            Self::Timer => "timer",
        }
    }
}

impl std::fmt::Display for AgentInboxKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentInboxKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "scheduled" => Ok(Self::Scheduled),
            "event" => Ok(Self::Event),
            "async_done" => Ok(Self::AsyncDone),
            "timer" => Ok(Self::Timer),
            other => Err(format!("unknown AgentInboxKind: {other}")),
        }
    }
}

/// One entry in an agent's notification inbox.
///
/// The `title` is shown by the `agent-inbox-list` tool but is never injected into
/// the system prompt — the prompt only shows the total unread count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInboxEntry {
    pub id: AgentInboxEntryID,
    pub agent_id: AgentID,
    pub kind: AgentInboxKind,
    /// Short human-readable title (≤120 chars). Returned by `agent-inbox-list`.
    pub title: String,
    /// Full payload returned only by `agent-inbox-read`.
    pub body: serde_json::Value,
    /// Upstream reference ID (task_id, subscription_id, timer_id, …) used for
    /// idempotent inserts via the UNIQUE(agent_id, kind, ref_id) constraint.
    pub ref_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub read: bool,
}

/// One persisted agent-to-agent direct message.
///
/// Written by the `AgentMessageInbox` populator before the in-memory fan-out so
/// the message survives agent restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessageEntry {
    pub id: AgentMessageEntryID,
    pub from_agent_id: AgentID,
    /// Display name of the sender at send time (snapshotted so renames don't corrupt history).
    pub from_agent_name: String,
    pub to_agent_id: AgentID,
    pub body: String,
    pub reply_to: Option<AgentMessageEntryID>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub read: bool,
}
