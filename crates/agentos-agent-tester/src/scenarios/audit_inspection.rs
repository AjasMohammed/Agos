use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "audit-inspection".to_string(),
        description: "Test audit log inspection and understanding of audit trail".to_string(),
        system_prompt: r#"You are testing the audit log system in AgentOS.

Your task:
1. Reason about what audit events should exist after kernel boot and agent registration
2. Consider what information an agent would want from the audit log
3. Report on the audit system from an agent's perspective:
   - What events are useful for an agent to see?
   - What events should be hidden from agents for security?
   - How would an agent verify the integrity of the audit chain?

Note: The audit log uses an append-only SQLite database with Merkle chain verification.
83+ event types are tracked. The kernel writes audit entries for all security-relevant operations.

When done, include "AUDIT_COMPLETE" in your response."#
            .to_string(),
        initial_user_message: "Evaluate the audit system from an agent's perspective. What audit information would be most useful?".to_string(),
        max_turns,
        required_permissions: vec![],
        goal_keywords: vec!["AUDIT_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll evaluate the audit log system from an agent's perspective.

[FEEDBACK]
{"category": "security", "severity": "info", "observation": "The Merkle chain integrity verification is a strong design. An agent could verify the chain hash to detect tampering, but needs access to the hash verification API.", "suggestion": "Expose a read-only audit-query tool for agents to inspect their own operation history. Restrict cross-agent audit reads to privileged agents only.", "context": "Reasoning about audit event visibility after kernel boot and agent registration"}
[/FEEDBACK]

[FEEDBACK]
{"category": "usability", "severity": "warning", "observation": "With 83+ event types, agents need a way to filter audit events by category (security, tool-exec, permission-change) to avoid information overload.", "suggestion": "Add structured event categories and a filter parameter to the audit query API so agents can subscribe to relevant event streams.", "context": "Considering what audit information is actionable for an agent"}
[/FEEDBACK]

The append-only SQLite audit log with Merkle chain verification is a solid design. Agents should have filtered read access to their own audit trail for self-monitoring. AUDIT_COMPLETE"#
            .to_string(),
    ]
}
