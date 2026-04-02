use agentos_types::TaskState;

/// Map a `TaskState` variant to its lowercase string representation used in
/// API responses and WebSocket events.
pub(crate) fn task_state_str(s: &TaskState) -> &str {
    match s {
        TaskState::Queued => "queued",
        TaskState::Running => "running",
        TaskState::Waiting => "waiting",
        TaskState::Suspended => "suspended",
        TaskState::Complete => "complete",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}
