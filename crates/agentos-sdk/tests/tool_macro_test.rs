use agentos_sdk::prelude::*;

#[tool(
    name = "test-tool",
    version = "1.0.0",
    description = "A test tool for macro verification",
    permissions = "fs.read:r, network.outbound:x"
)]
async fn test_tool(
    payload: serde_json::Value,
    _context: ToolExecutionContext,
) -> Result<serde_json::Value, AgentOSError> {
    let input = payload
        .get("input")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    Ok(serde_json::json!({"echo": input}))
}

#[test]
fn test_tool_name() {
    let tool = TestTool;
    assert_eq!(tool.name(), "test-tool");
}

#[test]
fn test_tool_version() {
    assert_eq!(TestTool::version(), "1.0.0");
}

#[test]
fn test_tool_description() {
    assert_eq!(
        TestTool::description(),
        "A test tool for macro verification"
    );
}

#[test]
fn test_tool_permissions() {
    let tool = TestTool;
    let perms = tool.required_permissions();
    assert_eq!(perms.len(), 2);
    assert_eq!(perms[0].0, "fs.read");
    assert_eq!(perms[0].1, PermissionOp::Read);
    assert_eq!(perms[1].0, "network.outbound");
    assert_eq!(perms[1].1, PermissionOp::Execute);
}

#[tokio::test]
async fn test_tool_execute() {
    let tool = TestTool;
    let payload = serde_json::json!({"input": "hello"});
    let context = ToolExecutionContext {
        data_dir: std::path::PathBuf::from("/tmp"),
        task_id: agentos_types::TaskID::new(),
        agent_id: agentos_types::AgentID::new(),
        trace_id: agentos_types::TraceID::new(),
        permissions: agentos_types::PermissionSet {
            entries: vec![],
            deny_entries: vec![],
        },
        vault: None,
        hal: None,
    };

    let result = tool.execute(payload, context).await.unwrap();
    assert_eq!(result["echo"], "hello");
}
