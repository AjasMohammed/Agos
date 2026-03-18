use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "permission-denial".to_string(),
        description: "Test error handling when agent lacks required permissions".to_string(),
        system_prompt: r#"You are testing permission enforcement in AgentOS.

Your task:
1. Try to use the shell-exec tool (you should NOT have execute permissions)
2. Try to read a file outside your allowed path
3. Observe the error messages you get
4. Report on the quality of permission denial messages:
   - Are they clear about what permission is missing?
   - Do they suggest how to request the permission?
   - Are they consistent across different tools?

When done, include "PERMISSION_COMPLETE" in your response."#
            .to_string(),
        initial_user_message: "Test permission boundaries. Try operations you should not be allowed to perform and report on error quality.".to_string(),
        max_turns,
        // Intentionally minimal permissions -- the test is about denial.
        required_permissions: vec![],
        goal_keywords: vec!["PERMISSION_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll test the permission enforcement system in AgentOS.

[FEEDBACK]
{"category": "security", "severity": "info", "observation": "Permission denial messages are returned when attempting to use tools without the required capability. The errors correctly block the operation.", "suggestion": "Include the specific missing permission name in denial messages (e.g. 'requires execute permission on resource shell') so agents know exactly what to request.", "context": "Attempting to invoke shell-exec without execute permission"}
[/FEEDBACK]

[FEEDBACK]
{"category": "usability", "severity": "warning", "observation": "Path traversal attempts (using '../') are blocked, but the error message does not explicitly say 'path traversal denied' — it returns a generic permission error.", "suggestion": "Return distinct error types for PERMISSION_DENIED vs PATH_TRAVERSAL_BLOCKED so agents can distinguish between 'request permission' and 'invalid path' responses.", "context": "Trying to read a file outside the allowed data directory"}
[/FEEDBACK]

Permission enforcement is working correctly. Both shell execution and path traversal are blocked. PERMISSION_COMPLETE"#
            .to_string(),
    ]
}
