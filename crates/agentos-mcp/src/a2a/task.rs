/// A2A Task — the unit of work delegated between agents.
///
/// Lifecycle:  submitted → working → completed | failed | cancelled
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A task delegated from an external agent to this AgentOS agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2ATask {
    /// Unique task identifier (UUIDv4).
    pub id: String,

    /// URL of the agent that sent this task (their Agent Card URL).
    pub sender: String,

    /// Which capability to invoke (must match a capability in the Agent Card).
    pub capability: String,

    /// Task input payload (validated against the capability's input_schema).
    pub input: serde_json::Value,

    /// Current task status.
    pub status: A2ATaskStatus,

    /// When the task was submitted.
    pub created_at: DateTime<Utc>,

    /// When the task last changed status.
    pub updated_at: DateTime<Utc>,
}

/// The lifecycle state of an A2A task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum A2ATaskStatus {
    /// Task received; awaiting execution.
    Submitted,

    /// Task is currently being processed.
    Working {
        /// Optional progress message.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Task completed successfully.
    Completed {
        /// Task output payload.
        output: serde_json::Value,
    },

    /// Task failed.
    Failed {
        /// Human-readable error description.
        error: String,
    },

    /// Task was cancelled.
    Cancelled,
}

impl A2ATask {
    pub fn new(id: String, sender: String, capability: String, input: serde_json::Value) -> Self {
        let now = Utc::now();
        Self {
            id,
            sender,
            capability,
            input,
            status: A2ATaskStatus::Submitted,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            &self.status,
            A2ATaskStatus::Completed { .. }
                | A2ATaskStatus::Failed { .. }
                | A2ATaskStatus::Cancelled
        )
    }
}

/// Request body for submitting a new A2A task.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubmitTaskRequest {
    pub sender: String,
    pub capability: String,
    pub input: serde_json::Value,
}

/// Request body for the cancel endpoint (empty, but typed for clarity).
#[derive(Debug, Deserialize)]
pub struct CancelTaskRequest {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_task_starts_submitted() {
        let t = A2ATask::new(
            "id-1".into(),
            "http://caller.example.com".into(),
            "file-read".into(),
            serde_json::json!({"path": "/tmp/x"}),
        );
        assert!(matches!(t.status, A2ATaskStatus::Submitted));
        assert!(!t.is_terminal());
    }

    #[test]
    fn completed_task_is_terminal() {
        let mut t = A2ATask::new(
            "id-2".into(),
            "x".into(),
            "y".into(),
            serde_json::Value::Null,
        );
        t.status = A2ATaskStatus::Completed {
            output: serde_json::json!({"result": "ok"}),
        };
        assert!(t.is_terminal());
    }

    #[test]
    fn task_status_serializes_with_state_tag() {
        let status = A2ATaskStatus::Failed {
            error: "something went wrong".into(),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["state"], "failed");
        assert!(json["error"].as_str().is_some());
    }
}
