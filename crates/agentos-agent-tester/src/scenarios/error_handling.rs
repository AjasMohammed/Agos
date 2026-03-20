use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "error-handling".to_string(),
        description: "Test error conditions: invalid tool names, malformed inputs, timeouts"
            .to_string(),
        system_prompt: r#"You are testing error handling in AgentOS.

Your task:
1. Try to call a tool that does not exist ("nonexistent-tool")
2. Try to call file-reader with invalid JSON input
3. Try to call file-reader with a missing required field (no "path")
4. Try to read a file that does not exist ("does-not-exist.txt")
5. For each error, report:
   - Was the error message helpful?
   - Did it tell you what went wrong?
   - Did it suggest how to fix the issue?
   - Was the error type/category appropriate?

When done, include "ERRORS_COMPLETE" in your response."#
            .to_string(),
        initial_user_message: "Test various error conditions. Intentionally make mistakes and evaluate the error messages.".to_string(),
        max_turns,
        required_permissions: vec!["fs.user_data:rw".to_string()],
        goal_keywords: vec!["ERRORS_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll test error handling in AgentOS by triggering various failure conditions.

[FEEDBACK]
{"category": "usability", "severity": "warning", "observation": "Calling a non-existent tool returns an error, but the message only says 'tool not found' without listing available tools. Agents cannot self-correct without knowing what tools are valid.", "suggestion": "Include the list of registered tool names in the 'tool not found' error, or provide a fuzzy match suggestion (e.g. 'did you mean file-reader?').", "context": "Calling 'nonexistent-tool' to test unknown tool error handling"}
[/FEEDBACK]

[FEEDBACK]
{"category": "correctness", "severity": "info", "observation": "Missing required fields in tool input are caught with a descriptive error naming the missing field. This is helpful and allows agents to self-correct.", "suggestion": "Add field-level validation errors with JSON path notation (e.g. 'missing required field: input.path') for complex nested inputs.", "context": "Calling file-reader without the required 'path' field"}
[/FEEDBACK]

[FEEDBACK]
{"category": "usability", "severity": "info", "observation": "Reading a non-existent file returns a clear 'file not found' error. The error is accurate and actionable.", "suggestion": null, "context": "Reading does-not-exist.txt to test file not found error"}
[/FEEDBACK]

Error handling across all tested conditions is functional. Some improvements to error messages would help agents self-correct faster. ERRORS_COMPLETE"#
            .to_string(),
    ]
}
