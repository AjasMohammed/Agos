use serde::{Deserialize, Serialize};

pub mod agent_lifecycle;
pub mod audit_inspection;
pub mod error_handling;
pub mod file_io;
pub mod memory_rw;
pub mod permission_denial;
pub mod pipeline_exec;
pub mod secret_management;
pub mod tool_discovery;
pub mod web_ui;

/// Return all built-in test scenarios covering every major AgentOS subsystem.
pub fn builtin_scenarios(max_turns: usize) -> Vec<TestScenario> {
    vec![
        agent_lifecycle::scenario(max_turns),
        tool_discovery::scenario(max_turns),
        file_io::scenario(max_turns),
        memory_rw::scenario(max_turns),
        pipeline_exec::scenario(max_turns),
        secret_management::scenario(max_turns),
        permission_denial::scenario(max_turns),
        audit_inspection::scenario(max_turns),
        error_handling::scenario(max_turns),
        web_ui::scenario(max_turns),
    ]
}

/// Return scenarios filtered by name.
pub fn filter_scenarios(names: &[String], max_turns: usize) -> Vec<TestScenario> {
    builtin_scenarios(max_turns)
        .into_iter()
        .filter(|s| names.iter().any(|n| n == &s.name))
        .collect()
}

/// Return the canned mock responses for a scenario identified by name.
/// Returns an empty `Vec` for unknown scenario names.
pub fn mock_responses_for(name: &str) -> Vec<String> {
    match name {
        "agent-lifecycle" => agent_lifecycle::mock_responses(),
        "tool-discovery" => tool_discovery::mock_responses(),
        "file-io" => file_io::mock_responses(),
        "memory-rw" => memory_rw::mock_responses(),
        "pipeline-exec" => pipeline_exec::mock_responses(),
        "secret-management" => secret_management::mock_responses(),
        "permission-denial" => permission_denial::mock_responses(),
        "audit-inspection" => audit_inspection::mock_responses(),
        "error-handling" => error_handling::mock_responses(),
        "web-ui" => web_ui::mock_responses(),
        _ => vec![],
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestScenario {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub initial_user_message: String,
    pub max_turns: usize,
    pub required_permissions: Vec<String>,
    /// If LLM response contains any of these keywords, the goal is considered met.
    pub goal_keywords: Vec<String>,
}

/// Per-turn timing and cost metrics captured during scenario execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMetrics {
    pub turn: usize,
    pub inference_ms: u64,
    pub tool_execution_ms: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub tool_called: Option<String>,
    /// `Some(true)` = tool call succeeded, `Some(false)` = tool call failed,
    /// `None` = no tool call was made this turn.
    pub tool_succeeded: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub scenario_name: String,
    pub outcome: ScenarioOutcome,
    pub turns_used: usize,
    pub max_turns: usize,
    pub tool_calls_made: usize,
    pub feedback_count: usize,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub duration_ms: u64,
    pub error_message: Option<String>,
    pub turn_metrics: Vec<TurnMetrics>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioOutcome {
    Complete,
    Incomplete,
    Errored,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_scenarios_returns_ten() {
        assert_eq!(builtin_scenarios(10).len(), 10);
    }

    #[test]
    fn all_scenarios_have_required_fields() {
        for s in builtin_scenarios(10) {
            assert!(!s.name.is_empty(), "{}: name must not be empty", s.name);
            assert!(
                !s.description.is_empty(),
                "{}: description must not be empty",
                s.name
            );
            assert!(
                !s.system_prompt.is_empty(),
                "{}: system_prompt must not be empty",
                s.name
            );
            assert!(
                !s.initial_user_message.is_empty(),
                "{}: initial_user_message must not be empty",
                s.name
            );
            assert!(
                !s.goal_keywords.is_empty(),
                "{}: goal_keywords must not be empty",
                s.name
            );
        }
    }

    #[test]
    fn filter_scenarios_returns_matching() {
        let names = vec!["file-io".to_string()];
        let filtered = filter_scenarios(&names, 10);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "file-io");
    }

    #[test]
    fn filter_scenarios_returns_multiple() {
        let names = vec!["file-io".to_string(), "memory-rw".to_string()];
        let filtered = filter_scenarios(&names, 10);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_scenarios_returns_empty_for_unknown_name() {
        let names = vec!["nonexistent-scenario".to_string()];
        let filtered = filter_scenarios(&names, 10);
        assert!(filtered.is_empty());
    }

    #[test]
    fn mock_responses_for_returns_responses_for_all_known_names() {
        let expected_names = [
            "agent-lifecycle",
            "tool-discovery",
            "file-io",
            "memory-rw",
            "pipeline-exec",
            "secret-management",
            "permission-denial",
            "audit-inspection",
            "error-handling",
            "web-ui",
        ];
        for name in expected_names {
            let responses = mock_responses_for(name);
            assert!(
                !responses.is_empty(),
                "mock_responses_for('{}') must not be empty",
                name
            );
        }
    }

    #[test]
    fn mock_responses_for_returns_empty_for_unknown_name() {
        assert!(mock_responses_for("nonexistent").is_empty());
    }

    #[test]
    fn mock_responses_contain_goal_keywords() {
        let pairs: &[(&str, Vec<String>, Vec<String>)] = &[
            (
                "agent-lifecycle",
                agent_lifecycle::mock_responses(),
                agent_lifecycle::scenario(5).goal_keywords,
            ),
            (
                "tool-discovery",
                tool_discovery::mock_responses(),
                tool_discovery::scenario(5).goal_keywords,
            ),
            (
                "file-io",
                file_io::mock_responses(),
                file_io::scenario(5).goal_keywords,
            ),
            (
                "memory-rw",
                memory_rw::mock_responses(),
                memory_rw::scenario(5).goal_keywords,
            ),
            (
                "pipeline-exec",
                pipeline_exec::mock_responses(),
                pipeline_exec::scenario(5).goal_keywords,
            ),
            (
                "secret-management",
                secret_management::mock_responses(),
                secret_management::scenario(5).goal_keywords,
            ),
            (
                "permission-denial",
                permission_denial::mock_responses(),
                permission_denial::scenario(5).goal_keywords,
            ),
            (
                "audit-inspection",
                audit_inspection::mock_responses(),
                audit_inspection::scenario(5).goal_keywords,
            ),
            (
                "error-handling",
                error_handling::mock_responses(),
                error_handling::scenario(5).goal_keywords,
            ),
            (
                "web-ui",
                web_ui::mock_responses(),
                web_ui::scenario(5).goal_keywords,
            ),
        ];

        for (name, responses, keywords) in pairs {
            for response in responses {
                let lower = response.to_lowercase();
                let has_keyword = keywords.iter().any(|kw| lower.contains(&kw.to_lowercase()));
                assert!(
                    has_keyword,
                    "mock response for '{}' is missing a goal keyword (keywords: {:?})",
                    name, keywords
                );
            }
        }
    }
}
