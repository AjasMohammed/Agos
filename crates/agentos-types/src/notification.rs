use crate::ids::{AgentID, NotificationID, TaskID, TraceID};
use crate::task::TaskState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A message from an agent or kernel subsystem directed at the human user.
///
/// `UserMessage` is the single data model shared by all delivery channels.
/// It carries both fire-and-forget notifications and interactive questions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub id: NotificationID,
    pub from: NotificationSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskID>,
    pub trace_id: TraceID,
    pub kind: UserMessageKind,
    pub priority: NotificationPriority,
    /// Short summary ≤80 chars — used for CLI one-liners and email subjects.
    pub subject: String,
    /// Full markdown body.
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction: Option<InteractionRequest>,
    #[serde(default)]
    pub delivery_status: HashMap<String, DeliveryStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<UserResponse>,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub read: bool,
    /// Groups related messages in inbox (e.g. all messages from the same task).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// For channels that support reply threading (Telegram message_id, email In-Reply-To).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_external_id: Option<String>,
}

/// Who produced the message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "id")]
pub enum NotificationSource {
    Agent(AgentID),
    Kernel,
    System,
}

/// The semantic category of the message, carrying structured payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum UserMessageKind {
    Notification,
    Question {
        question: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        options: Option<Vec<String>>,
        #[serde(default = "default_true")]
        free_text_allowed: bool,
    },
    TaskComplete {
        task_id: TaskID,
        outcome: TaskOutcome,
        summary: String,
        duration_ms: u64,
        iterations: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        tool_calls: u32,
    },
    StatusUpdate {
        task_id: TaskID,
        old_state: TaskState,
        new_state: TaskState,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

fn default_true() -> bool {
    true
}

/// Importance level — adapters may filter on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationPriority {
    Info,
    Warning,
    Urgent,
    Critical,
}

impl std::fmt::Display for NotificationPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotificationPriority::Info => write!(f, "info"),
            NotificationPriority::Warning => write!(f, "warning"),
            NotificationPriority::Urgent => write!(f, "urgent"),
            NotificationPriority::Critical => write!(f, "critical"),
        }
    }
}

/// Parameters controlling blocking interaction behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRequest {
    pub timeout_secs: u64,
    /// Text returned to the agent when the timeout fires with no user response.
    pub auto_action: String,
    pub blocking: bool,
    /// Max concurrent blocking questions from one agent (default 3).
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u8,
}

fn default_max_concurrent() -> u8 {
    3
}

/// A reply from the user to a `Question` message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserResponse {
    pub text: String,
    pub responded_at: DateTime<Utc>,
    pub channel: DeliveryChannel,
}

/// A delivery channel identifier.
///
/// String-backed so that new channels can be added without enum exhaustiveness
/// updates across the codebase. Well-known values are provided as constants on
/// `DeliveryChannel` (`CLI`, `WEB`, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeliveryChannel(pub String);

impl DeliveryChannel {
    pub const CLI: &'static str = "cli";
    pub const WEB: &'static str = "web";
    pub const WEBHOOK: &'static str = "webhook";
    pub const DESKTOP: &'static str = "desktop";
    pub const SLACK: &'static str = "slack";
    pub const TELEGRAM: &'static str = "telegram";
    pub const NTFY: &'static str = "ntfy";
    pub const EMAIL: &'static str = "email";

    pub fn cli() -> Self {
        Self(Self::CLI.to_string())
    }
    pub fn web() -> Self {
        Self(Self::WEB.to_string())
    }
    pub fn webhook() -> Self {
        Self(Self::WEBHOOK.to_string())
    }
    pub fn custom(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DeliveryChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Whether a delivery attempt succeeded, failed, or was skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum DeliveryStatus {
    Pending,
    Delivered { at: DateTime<Utc> },
    Failed { reason: String },
    Skipped,
}

/// Terminal outcome of a completed task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Success,
    Failed,
    Cancelled,
    TimedOut,
}

impl std::fmt::Display for TaskOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskOutcome::Success => write!(f, "success"),
            TaskOutcome::Failed => write!(f, "failed"),
            TaskOutcome::Cancelled => write!(f, "cancelled"),
            TaskOutcome::TimedOut => write!(f, "timed_out"),
        }
    }
}
