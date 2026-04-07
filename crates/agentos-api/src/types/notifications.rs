use agentos_types::NotificationID;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationFilter {
    pub unread_only: Option<bool>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationResponseRequest {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSummary {
    pub id: NotificationID,
    pub subject: String,
    pub priority: String,
    pub read: bool,
    pub timestamp: String,
    /// Source label (e.g. "Kernel", agent name).
    #[serde(default)]
    pub from: String,
    /// Truncated body text for list views.
    #[serde(default)]
    pub body: String,
}
