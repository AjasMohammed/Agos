use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "web-ui".to_string(),
        description: "Evaluate the web UI design from an agent perspective (conceptual)"
            .to_string(),
        system_prompt: r#"You are evaluating the AgentOS web UI from an agent's perspective.

AgentOS includes a web UI (Axum + HTMX) that provides:
- Dashboard showing connected agents, active tasks, system health
- Agent management (connect, disconnect, view status)
- Task management (run, cancel, view logs)
- Tool management (list, install, remove)
- Audit log viewer
- Secrets management
- Pipeline management

Your task:
1. Consider what API endpoints an agent would need to interact with the web UI programmatically
2. Evaluate whether the web UI's REST endpoints are agent-friendly (JSON responses vs HTML)
3. Suggest improvements for making the web UI usable by both human operators and AI agents
4. Consider authentication and authorization for web UI access

When done, include "WEBUI_COMPLETE" in your response."#
            .to_string(),
        initial_user_message: "Evaluate the web UI design from an agent's perspective. What would you need from an API to manage agents and tasks programmatically?".to_string(),
        max_turns,
        required_permissions: vec![],
        goal_keywords: vec!["WEBUI_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll evaluate the AgentOS web UI from an agent's perspective.

[FEEDBACK]
{"category": "usability", "severity": "warning", "observation": "The web UI uses HTMX which returns HTML fragments rather than JSON. This is great for human operators but makes it difficult for agents to consume the API programmatically.", "suggestion": "Add a parallel JSON API under /api/v1/ that mirrors the web UI endpoints. Use content negotiation (Accept: application/json) to serve JSON to agents and HTML to browsers from the same routes.", "context": "Evaluating REST endpoint response format for agent-programmatic access"}
[/FEEDBACK]

[FEEDBACK]
{"category": "security", "severity": "info", "observation": "Web UI access authentication is not yet defined for agents. Agents should use capability tokens for API authentication, separate from human session cookies.", "suggestion": "Implement bearer token authentication for the JSON API endpoints, using the same capability token system used for tool execution.", "context": "Considering authentication for agent access to web UI management endpoints"}
[/FEEDBACK]

[FEEDBACK]
{"category": "ergonomics", "severity": "info", "observation": "The dashboard covers all major subsystems (agents, tasks, tools, audit, secrets, pipelines), which gives good operational visibility. Server-Sent Events for real-time updates would be very useful for agents monitoring task progress.", "suggestion": "Expose SSE streams for task log tailing and agent status changes so agents can react to system events without polling.", "context": "Reviewing the web UI feature set for programmatic agent interaction"}
[/FEEDBACK]

The web UI has good coverage of AgentOS subsystems. Adding a JSON API layer and capability token authentication would make it accessible to agents. WEBUI_COMPLETE"#
            .to_string(),
    ]
}
