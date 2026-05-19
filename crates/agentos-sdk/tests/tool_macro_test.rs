use agentos_sdk::prelude::*;
use serde::{Deserialize, Serialize};

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

/// Strongly-typed input used to verify `#[tool(input = T)]` auto-derives a
/// `payload_schema()` constructor via schemars.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EchoInput {
    /// Message to echo back to the caller.
    pub message: String,
    /// Optional repeat count (default 1).
    #[serde(default)]
    pub repeat: Option<u32>,
}

#[tool(
    name = "echo-tool",
    version = "1.0.0",
    description = "A typed test tool for schemars verification",
    permissions = "fs.read:r",
    input = EchoInput,
)]
async fn echo_tool(
    payload: serde_json::Value,
    _context: ToolExecutionContext,
) -> Result<serde_json::Value, AgentOSError> {
    Ok(payload)
}

#[test]
fn typed_tool_emits_payload_schema() {
    let schema = EchoTool::payload_schema();
    assert!(schema.is_object(), "schema must be a JSON object");
    let props = schema
        .pointer("/properties")
        .expect("properties present")
        .as_object()
        .expect("properties is an object");
    assert!(props.contains_key("message"));
    assert!(props.contains_key("repeat"));
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
        file_lock_registry: None,
        agent_registry: None,
        task_registry: None,
        escalation_query: None,
        workspace_paths: vec![],
        workspace_paths_writable: vec![],
        workspace_paths_executable: vec![],
        capability_registry: None,
        capability_dispatcher: None,
        storage_zone_query: None,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tool_categories: None,
    };

    let result = tool.execute(payload, context).await.unwrap();
    assert_eq!(result["echo"], "hello");
}
