use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "tool-discovery".to_string(),
        description:
            "Test tool listing, manifest inspection, and understanding tool capabilities"
                .to_string(),
        system_prompt: r#"You are testing tool discovery in AgentOS.

Your task:
1. Review the list of available tools provided in the system context
2. Try to understand what each tool does from its description
3. Identify any tools whose purpose or usage is unclear
4. Try calling one tool (file-reader) to verify it works
5. Report on the quality of tool descriptions and discoverability

When done, include "TOOLS_DISCOVERED" in your response."#
            .to_string(),
        initial_user_message: "Review the available tools and test the file-reader tool. Create a test file first using file-writer, then read it back.".to_string(),
        max_turns,
        required_permissions: vec!["fs.user_data".to_string()],
        goal_keywords: vec!["TOOLS_DISCOVERED".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I've reviewed the available tools in AgentOS.

[FEEDBACK]
{"category": "usability", "severity": "info", "observation": "Tool descriptions are present but could benefit from usage examples inline. The file-reader and file-writer tools are clear in purpose, but parameter names could be more descriptive.", "suggestion": "Add a short usage example to each tool's description field in the manifest so agents can immediately understand input format.", "context": "Reviewing tool manifest descriptions for discoverability"}
[/FEEDBACK]

I have inspected the tool list and found the file-reader and file-writer tools. Tool discovery works correctly — the manifest system provides enough information to understand tool capabilities. TOOLS_DISCOVERED"#
            .to_string(),
    ]
}
