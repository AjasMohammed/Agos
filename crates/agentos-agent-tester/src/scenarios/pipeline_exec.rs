use super::TestScenario;

pub fn scenario(max_turns: usize) -> TestScenario {
    TestScenario {
        name: "pipeline-exec".to_string(),
        description: "Test multi-step pipeline definition and execution".to_string(),
        system_prompt: r#"You are testing the pipeline execution system in AgentOS.

Your task:
1. Define a simple 2-step pipeline in YAML:
   - Step 1: Write a file "pipeline-output.txt" with content "Pipeline step 1"
   - Step 2: Read the file back
2. Install the pipeline
3. Run the pipeline
4. Check the pipeline status
5. Report on the pipeline definition format, error messages, and execution feedback

Note: Pipeline YAML format:
```yaml
name: test-pipeline
description: A test pipeline
steps:
  - name: write-step
    tool: file-writer
    input:
      path: pipeline-output.txt
      content: "Pipeline step 1"
  - name: read-step
    tool: file-reader
    input:
      path: pipeline-output.txt
    depends_on:
      - write-step
```

When done, include "PIPELINE_COMPLETE" in your response."#
            .to_string(),
        initial_user_message: "Begin the pipeline test. Note that pipeline operations use the kernel API, not tools. Describe what you would do and provide feedback on the pipeline system design.".to_string(),
        max_turns,
        required_permissions: vec![
            "fs.user_data".to_string(),
            "pipeline.execute".to_string(),
        ],
        goal_keywords: vec!["PIPELINE_COMPLETE".to_string()],
    }
}

pub fn mock_responses() -> Vec<String> {
    vec![
        r#"I'll evaluate the pipeline execution system in AgentOS.

[FEEDBACK]
{"category": "usability", "severity": "info", "observation": "The YAML pipeline definition format is clear and intuitive. The depends_on field for step ordering is explicit and easy to understand.", "suggestion": "Provide a pipeline-validate command that checks YAML syntax and tool names before installation, to catch errors early.", "context": "Reviewing the pipeline YAML format for the test-pipeline definition"}
[/FEEDBACK]

[FEEDBACK]
{"category": "ergonomics", "severity": "warning", "observation": "Pipelines are managed through the kernel API rather than agent tools, which creates a gap in the agent's ability to interact with pipelines autonomously.", "suggestion": "Add pipeline-install and pipeline-run as first-class agent tools so pipelines can be triggered from within agent tool calls.", "context": "Attempting to install and run a pipeline from an agent context"}
[/FEEDBACK]

I have evaluated the pipeline system. The YAML definition format is well-designed and the step dependency model is clear. PIPELINE_COMPLETE"#
            .to_string(),
    ]
}
