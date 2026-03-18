use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "file-io".to_string(),
        description: "Test file reading, writing, directory listing, and path handling".to_string(),
        system_prompt: r#"You are testing file I/O operations in AgentOS.

Your task:
1. Write a file named "test-output.txt" with the content "Hello from AgentOS test"
2. Read the file back and verify the content matches
3. List the directory contents to see the file
4. Try to write to a nested subdirectory path ("subdir/nested.txt")
5. Try to read a non-existent file and observe the error
6. Report on error message quality, path handling, and overall file I/O ergonomics

Use the file-writer and file-reader tools.

When done, include "FILE_IO_COMPLETE" in your response."#
            .to_string(),
        initial_user_message: "Start the file I/O test. Write a test file, then read it back."
            .to_string(),
        max_turns,
        required_permissions: vec!["fs.user_data".to_string()],
        goal_keywords: vec!["FILE_IO_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll test the file I/O system in AgentOS.

[FEEDBACK]
{"category": "usability", "severity": "info", "observation": "File write and read operations work correctly. The path is scoped to the agent data directory, which is a good security boundary.", "suggestion": "Directory listing would be a useful tool addition. Currently there is no built-in way to enumerate files without using the shell.", "context": "Testing file-writer and file-reader tools with test-output.txt"}
[/FEEDBACK]

[FEEDBACK]
{"category": "ergonomics", "severity": "warning", "observation": "Attempting to read a non-existent file returns a generic error message that doesn't distinguish between 'permission denied' and 'file not found'.", "suggestion": "Return structured error codes (e.g. NOT_FOUND, PERMISSION_DENIED) so agents can handle different failure modes programmatically.", "context": "Reading does-not-exist.txt to test error handling"}
[/FEEDBACK]

I have completed the file I/O test: wrote test-output.txt, read it back successfully, tested nested path creation, and observed error behavior. FILE_IO_COMPLETE"#
            .to_string(),
    ]
}
