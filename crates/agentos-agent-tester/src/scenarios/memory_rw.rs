use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "memory-rw".to_string(),
        description: "Test semantic and episodic memory write and search".to_string(),
        system_prompt: r#"You are testing the memory subsystem in AgentOS.

Your task:
1. Write a semantic memory entry about "Q1 revenue was 2.5 million dollars" with key "q1-revenue"
2. Search for it using a query like "revenue earnings"
3. Write an episodic memory entry about "Agent completed file I/O test successfully" with scope "episodic"
4. Search for the episodic entry
5. Report on memory search quality (relevance of results, latency, ease of use)

Use the memory-write and memory-search tools.

When done, include "MEMORY_COMPLETE" in your response."#
            .to_string(),
        initial_user_message:
            "Begin the memory test. Write a semantic memory entry, then search for it."
                .to_string(),
        max_turns,
        required_permissions: vec![
            "memory.semantic:rw".to_string(),
            "memory.episodic:rw".to_string(),
        ],
        goal_keywords: vec!["MEMORY_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll test the memory subsystem in AgentOS.

[FEEDBACK]
{"category": "usability", "severity": "info", "observation": "Semantic memory write accepts key-value pairs and the search returns relevant results. The embedding-based search correctly surfaces the 'q1-revenue' entry when querying for 'revenue earnings'.", "suggestion": "Expose a memory-list tool so agents can enumerate stored entries without needing a specific search query.", "context": "Writing and searching semantic memory with key 'q1-revenue'"}
[/FEEDBACK]

[FEEDBACK]
{"category": "performance", "severity": "info", "observation": "Memory search latency is acceptable for interactive use. Episodic memory search returns results ranked by recency which is appropriate for episodic scope.", "suggestion": "Consider exposing similarity scores in search results so agents can threshold on relevance quality.", "context": "Searching episodic memory for file I/O test completion entry"}
[/FEEDBACK]

Both semantic and episodic memory operations work correctly. Write and search operations are functional and the results are relevant. MEMORY_COMPLETE"#
            .to_string(),
    ]
}
