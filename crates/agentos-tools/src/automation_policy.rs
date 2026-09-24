//! Which tools may be invoked by automation rather than by a live agent turn.
//!
//! Scheduling is a powerful primitive: any tool that schedules or spawns work
//! could form a recursive bomb if scheduled itself. The same is true of a
//! stored procedure, which is a bag of tools fired by one call — so the list
//! lives here, in the crate both the scheduler and `procedure-create` can
//! reach, rather than kernel-side where only the scheduler could see it.
//!
//! **This is a name list, not a capability gate, and it cannot be one.**
//! `shell-exec` is not on it and must not be — it is a general-purpose tool
//! agents legitimately automate — but a step that can run a shell can reach
//! `agentos task run` or rewrite `procedural_memory.db` directly, which brings
//! back both spawning and self-triggering under a different name. What actually
//! bounds a step is the per-step capability token minted from the running
//! agent's own `PermissionSet`, plus the `ToolPre` approval gate. This list
//! only removes the obvious recursions that would be a foot-gun even for an
//! agent acting in good faith.
//!
//! `agentos-kernel`'s `schedule_action_policy` re-exports this.

const AUTOMATION_TOOL_DENYLIST: &[&str] = &[
    // Recursive scheduling primitives.
    "schedule-once",
    "schedule-recurring",
    "schedule-control",
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
    "task-spawn-async",
    "task-delegate",
    "start-conversation",
    "agent-call",
    "await-agents",
    "cancel-agent",
    "a2a-delegate",
    // Running a stored procedure is scheduling with one more level of
    // indirection: without this a procedure could call itself, or two could
    // call each other, and neither the scheduler's depth accounting nor the
    // capability TTL would notice.
    "procedure-run",
    // Authoring primitives: an unattended procedure that writes procedures
    // seeds recipes a later agent finds through procedure-search, and one that
    // deletes them can remove the recipe an operator approved.
    "procedure-create",
    "procedure-delete",
    // Interactive — would deadlock or escalate without a user loop.
    "ask-user",
];

/// True if `tool_name` may NOT be invoked from automation (a schedule action or
/// a stored procedure step).
///
/// The name is normalized to the hyphenated spelling first. `ToolRunner::execute`
/// auto-corrects `_` → `-` at dispatch, so an exact match against the raw name
/// would let `spawn_agent` past the denylist and then run it as `spawn-agent`.
/// Normalizing here (rather than at the call site) keeps every present and
/// future caller covered.
pub fn is_tool_blocked_for_automation(tool_name: &str) -> bool {
    AUTOMATION_TOOL_DENYLIST.contains(&tool_name.replace('_', "-").as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduling_and_spawning_primitives_are_blocked() {
        for tool in [
            "schedule-recurring",
            "spawn-agent",
            "task-delegate",
            "ask-user",
        ] {
            assert!(is_tool_blocked_for_automation(tool), "{tool}");
        }
    }

    /// Recursion guard: a procedure step that runs a procedure.
    #[test]
    fn procedure_run_is_blocked() {
        assert!(is_tool_blocked_for_automation("procedure-run"));
        assert!(is_tool_blocked_for_automation("procedure_run"));
    }

    #[test]
    fn underscore_spelling_does_not_slip_past() {
        assert!(is_tool_blocked_for_automation("spawn_agent"));
    }

    #[test]
    fn ordinary_tools_are_allowed() {
        for tool in ["speak", "audio", "file-reader", "http-client"] {
            assert!(!is_tool_blocked_for_automation(tool), "{tool}");
        }
    }
}
