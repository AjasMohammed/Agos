use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "agent-lifecycle".to_string(),
        description: "Test agent registration, status checking, and identity verification"
            .to_string(),
        system_prompt: r#"You are testing the agent lifecycle in AgentOS.

Your task:
1. Confirm you are registered as an agent by checking your status
2. Verify you have an agent identity (Ed25519 public key)
3. List all registered agents to see who else is connected
4. Report any issues with the registration process

Use the available tools to accomplish this. If you encounter errors, report them as feedback.

When you have completed all steps, include the word "LIFECYCLE_COMPLETE" in your response."#
            .to_string(),
        initial_user_message:
            "Begin the agent lifecycle test. Start by checking your registration status."
                .to_string(),
        max_turns,
        required_permissions: vec![],
        goal_keywords: vec!["LIFECYCLE_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll check my agent status in AgentOS.

[FEEDBACK]
{"category": "usability", "severity": "info", "observation": "I'm registered as 'test-agent'. The registration process appears seamless and automatic on connect.", "suggestion": "Provide an explicit agent status tool so agents can verify their own identity without needing external confirmation.", "context": "Agent lifecycle check — verifying registration and Ed25519 identity"}
[/FEEDBACK]

I can confirm that I am registered as "test-agent" with an active Ed25519 public key for identity verification. The agent registration and identity verification process works correctly. LIFECYCLE_COMPLETE"#
            .to_string(),
    ]
}
