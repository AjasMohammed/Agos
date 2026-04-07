use agentos_types::{AgentID, TaskID, TaskState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventType {
    StateChanged,
    SubAgentSpawned,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub event_type: TaskEventType,
    pub task_id: TaskID,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentID>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskID>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_depth: Option<u8>,
    pub timestamp: DateTime<Utc>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_event_round_trip() {
        let event = TaskEvent {
            event_type: TaskEventType::SubAgentSpawned,
            task_id: TaskID::new(),
            agent_id: Some(AgentID::new()),
            agent_name: Some("coordinator".to_string()),
            parent_task_id: Some(TaskID::new()),
            state: Some(TaskState::Queued),
            spawn_depth: Some(1),
            timestamp: Utc::now(),
            message: "spawned".to_string(),
        };

        let json = serde_json::to_string(&event).unwrap();
        let decoded: TaskEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.event_type, TaskEventType::SubAgentSpawned);
        assert_eq!(decoded.message, "spawned");
        assert_eq!(decoded.state, Some(TaskState::Queued));
    }
}
