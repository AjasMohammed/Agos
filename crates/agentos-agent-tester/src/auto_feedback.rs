use crate::feedback::{FeedbackCategory, FeedbackEntry, FeedbackSeverity};

/// Generate automatic feedback when a tool call fails.
pub fn feedback_from_tool_error(
    scenario: &str,
    turn: usize,
    tool_name: &str,
    error: &str,
) -> FeedbackEntry {
    let severity = if error.contains("PermissionDenied") {
        FeedbackSeverity::Warning
    } else {
        FeedbackSeverity::Error
    };

    let category = if error.contains("PermissionDenied") {
        FeedbackCategory::Security
    } else if error.contains("not found") || error.contains("ToolNotFound") {
        FeedbackCategory::Usability
    } else {
        FeedbackCategory::Correctness
    };

    FeedbackEntry {
        scenario: scenario.to_string(),
        turn,
        category,
        severity,
        observation: format!("Tool '{}' failed with error: {}", tool_name, error),
        suggestion: Some(classify_error_suggestion(error)),
        context: Some(format!("Attempted to call tool '{}'", tool_name)),
    }
}

/// Generate feedback when LLM inference itself fails.
pub fn feedback_from_inference_error(scenario: &str, turn: usize, error: &str) -> FeedbackEntry {
    FeedbackEntry {
        scenario: scenario.to_string(),
        turn,
        category: FeedbackCategory::Correctness,
        severity: FeedbackSeverity::Error,
        observation: format!("LLM inference failed: {}", error),
        suggestion: Some("Check LLM connectivity and API key validity".to_string()),
        context: Some("During scenario execution".to_string()),
    }
}

/// Generate feedback when scenario times out (exhausts turns).
pub fn feedback_from_timeout(scenario: &str, max_turns: usize) -> FeedbackEntry {
    FeedbackEntry {
        scenario: scenario.to_string(),
        turn: max_turns,
        category: FeedbackCategory::Usability,
        severity: FeedbackSeverity::Warning,
        observation: format!(
            "Scenario did not complete within {} turns. The LLM could not achieve the goal.",
            max_turns
        ),
        suggestion: Some(
            "Consider whether the scenario goal is achievable with the available tools, \
             or improve tool/system prompt clarity"
                .to_string(),
        ),
        context: Some("Scenario turn budget exhausted".to_string()),
    }
}

/// Generate feedback when the harness fails to grant a required permission during scenario setup.
pub fn feedback_from_permission_grant_failure(scenario: &str, permission: &str) -> FeedbackEntry {
    FeedbackEntry {
        scenario: scenario.to_string(),
        turn: 0,
        category: FeedbackCategory::Correctness,
        severity: FeedbackSeverity::Error,
        observation: format!(
            "Harness failed to grant required permission '{}' — scenario setup incomplete",
            permission
        ),
        suggestion: Some(
            "Check api_grant_permission returns a valid result for this permission format"
                .to_string(),
        ),
        context: Some("Scenario setup".to_string()),
    }
}

fn classify_error_suggestion(error: &str) -> String {
    if error.contains("PermissionDenied") {
        "Error message should tell the agent which specific permission is needed and how to request it".to_string()
    } else if error.contains("path traversal") || error.contains("../") {
        "Path traversal denial is correctly enforced. Error message quality is good.".to_string()
    } else if error.contains("not found") {
        "Consider suggesting similar tool names or listing available tools in the error".to_string()
    } else {
        "Review error message for agent-friendliness: is it actionable?".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_error_permission_denied_is_security_warning() {
        let entry = feedback_from_tool_error(
            "test",
            1,
            "file-reader",
            "PermissionDenied { resource: \"fs\" }",
        );
        assert_eq!(entry.category, FeedbackCategory::Security);
        assert_eq!(entry.severity, FeedbackSeverity::Warning);
        assert!(entry.observation.contains("file-reader"));
        assert_eq!(entry.scenario, "test");
        assert_eq!(entry.turn, 1);
    }

    #[test]
    fn test_tool_error_tool_not_found_is_usability_error() {
        let entry =
            feedback_from_tool_error("test", 1, "nonexistent", "ToolNotFound: tool not found");
        assert_eq!(entry.category, FeedbackCategory::Usability);
        assert_eq!(entry.severity, FeedbackSeverity::Error);
        assert!(entry.observation.contains("nonexistent"));
    }

    #[test]
    fn test_tool_error_generic_is_correctness_error() {
        let entry = feedback_from_tool_error("test", 2, "some-tool", "Unexpected error occurred");
        assert_eq!(entry.category, FeedbackCategory::Correctness);
        assert_eq!(entry.severity, FeedbackSeverity::Error);
    }

    #[test]
    fn test_inference_error_is_correctness_error() {
        let entry = feedback_from_inference_error("test", 1, "Connection refused");
        assert_eq!(entry.category, FeedbackCategory::Correctness);
        assert_eq!(entry.severity, FeedbackSeverity::Error);
        assert!(entry.observation.contains("Connection refused"));
        assert_eq!(entry.scenario, "test");
        assert_eq!(entry.turn, 1);
    }

    #[test]
    fn test_timeout_is_usability_warning() {
        let entry = feedback_from_timeout("test", 10);
        assert_eq!(entry.category, FeedbackCategory::Usability);
        assert_eq!(entry.severity, FeedbackSeverity::Warning);
        assert_eq!(entry.turn, 10);
        assert!(entry.observation.contains("10 turns"));
    }
}
