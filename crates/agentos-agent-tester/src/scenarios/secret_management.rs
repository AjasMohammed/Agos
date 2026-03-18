use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "secret-management".to_string(),
        description: "Test secrets vault interaction: set, list, and revoke secrets".to_string(),
        system_prompt: r#"You are testing the secrets vault in AgentOS.

Your task:
1. Describe how you would store an API key as a secret
2. Consider the security implications of secret storage
3. Note whether secrets are accessible to agents or only via proxy tokens
4. Report on the secret management ergonomics from an agent perspective

Note: Secrets are managed through the kernel API (set/list/revoke), not through tools.
The vault uses AES-256-GCM encryption with Argon2id key derivation.
Agents should use proxy tokens rather than accessing raw secret values.

When done, include "SECRETS_COMPLETE" in your response."#
            .to_string(),
        initial_user_message: "Evaluate the secrets management system from an agent's perspective. What would you need to store API keys securely?".to_string(),
        max_turns,
        required_permissions: vec![],
        goal_keywords: vec!["SECRETS_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll evaluate the secrets management system from an agent's perspective.

[FEEDBACK]
{"category": "security", "severity": "info", "observation": "The vault uses AES-256-GCM with Argon2id key derivation, which is a strong choice. Agents receive proxy tokens rather than raw secret values, which is the correct zero-exposure design.", "suggestion": "Provide a way for agents to request a secret proxy token for a named secret without going through the kernel API directly, keeping the interface consistent.", "context": "Evaluating secret storage security model for API key management"}
[/FEEDBACK]

[FEEDBACK]
{"category": "ergonomics", "severity": "warning", "observation": "Secrets are managed through the kernel API rather than agent-accessible tools, which means an agent cannot autonomously manage its own secrets.", "suggestion": "Add a secret-request tool that lets agents request access to pre-approved secrets via a capability token flow, audited by the kernel.", "context": "Checking whether agents can set or retrieve secrets independently"}
[/FEEDBACK]

The secrets vault design is strong from a security standpoint. The proxy token model prevents direct secret exposure to agents. SECRETS_COMPLETE"#
            .to_string(),
    ]
}
