use crate::ids::{AgentID, NotificationID};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An asynchronous callback notification sent to an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentNotification {
    pub id: NotificationID,
    pub agent_id: AgentID,
    pub source: String, // e.g., "schedule.done", "event.fired", "spawn_async.done"
    pub subject: String, // Short summary shown in the prompt
    pub body: String,   // Detailed content read via tools
    pub created_at: DateTime<Utc>,
    pub read: bool,
}
