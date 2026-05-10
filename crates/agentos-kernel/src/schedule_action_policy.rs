//! Policy for which tools may be invoked via a `RunTool` schedule action.
//!
//! Scheduling is a powerful primitive: any tool that schedules or spawns work
//! could form a recursive bomb if scheduled itself. We deny those by name as a
//! belt-and-braces measure on top of the RiskClass check that lives in the
//! schedule tool surface.

const SCHEDULE_TOOL_DENYLIST: &[&str] = &[
    // Recursive scheduling primitives.
    "schedule-once",
    "cancel-once-job",
    "list-once-jobs",
    "set-timer",
    "cancel-timer",
    "list-timers",
    "set-cron",
    "cancel-cron",
    "list-crons",
    // Spawning primitives.
    "spawn-agent",
    "spawn-async",
    "agent-call",
    "await-agents",
    "cancel-agent",
    "a2a-delegate",
    // Interactive — would deadlock or escalate without a user loop.
    "ask-user",
];

/// True if `tool_name` may NOT be invoked from a `RunTool` schedule action.
pub fn is_tool_blocked_for_schedule(tool_name: &str) -> bool {
    SCHEDULE_TOOL_DENYLIST.contains(&tool_name)
}

/// Maximum size of a `tool_args` JSON payload.
pub const MAX_TOOL_ARGS_BYTES: usize = 16 * 1024;

/// True if the encoded args payload exceeds the size cap.
pub fn args_exceed_size_cap(args: &serde_json::Value) -> bool {
    serde_json::to_string(args)
        .map(|s| s.len() > MAX_TOOL_ARGS_BYTES)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_meta_tools_blocked() {
        assert!(is_tool_blocked_for_schedule("schedule-once"));
        assert!(is_tool_blocked_for_schedule("set-timer"));
        assert!(is_tool_blocked_for_schedule("spawn-agent"));
        assert!(is_tool_blocked_for_schedule("ask-user"));
    }

    #[test]
    fn ordinary_tools_not_blocked() {
        assert!(!is_tool_blocked_for_schedule("datetime"));
        assert!(!is_tool_blocked_for_schedule("notify-user"));
        assert!(!is_tool_blocked_for_schedule("file-read"));
    }

    #[test]
    fn args_size_cap_enforced() {
        let small = serde_json::json!({"k": "v"});
        assert!(!args_exceed_size_cap(&small));

        let big_str = "x".repeat(MAX_TOOL_ARGS_BYTES + 1);
        let big = serde_json::json!({"k": big_str});
        assert!(args_exceed_size_cap(&big));
    }
}
