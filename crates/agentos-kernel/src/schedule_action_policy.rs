//! Policy for which tools may be invoked via a `RunTool` schedule action.
//!
//! Scheduling is a powerful primitive: any tool that schedules or spawns work
//! could form a recursive bomb if scheduled itself. We deny those by name as a
//! belt-and-braces measure on top of the RiskClass check that lives in the
//! schedule tool surface.

pub use agentos_tools::automation_policy::is_tool_blocked_for_automation;

/// True if `tool_name` may NOT be invoked from a `RunTool` schedule action.
///
/// Thin alias over [`is_tool_blocked_for_automation`]. The list moved into
/// `agentos-tools` when stored procedures gained executable steps: a procedure
/// is a bag of tools fired by one call, so `procedure-create` has to consult
/// the same list, and it cannot depend on this crate.
pub fn is_tool_blocked_for_schedule(tool_name: &str) -> bool {
    is_tool_blocked_for_automation(tool_name)
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
        // Regression: the list named `spawn-async`, which is not a tool, so the
        // real `task-spawn-async` passed.
        assert!(is_tool_blocked_for_schedule("task-spawn-async"));
        assert!(is_tool_blocked_for_schedule("task-delegate"));
        assert!(is_tool_blocked_for_schedule("start-conversation"));
        assert!(is_tool_blocked_for_schedule("schedule-recurring"));
        assert!(is_tool_blocked_for_schedule("ask-user"));
    }

    /// C1 regression: the denylist was an exact match on the raw name while
    /// `ToolRunner::execute` resolves `_` → `-` afterwards, so every entry was
    /// bypassable by scheduling the underscore spelling.
    #[test]
    fn underscore_spelling_is_blocked() {
        assert!(is_tool_blocked_for_schedule("spawn_agent"));
        assert!(is_tool_blocked_for_schedule("schedule_once"));
        assert!(is_tool_blocked_for_schedule("set_cron"));
        assert!(is_tool_blocked_for_schedule("a2a_delegate"));
        assert!(is_tool_blocked_for_schedule("ask_user"));
    }

    #[test]
    fn ordinary_tools_not_blocked() {
        assert!(!is_tool_blocked_for_schedule("datetime"));
        assert!(!is_tool_blocked_for_schedule("notify-user"));
        assert!(!is_tool_blocked_for_schedule("file-read"));
        assert!(!is_tool_blocked_for_schedule("file_read"));
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
