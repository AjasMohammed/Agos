//! CVE class: path traversal / symlink escape from the agent's file sandbox.
//!
//! The control under test is `resolve_tool_path` plus canonical containment in
//! `file-reader`: relative paths resolve under `data_dir/agents/<id>/`, `..`
//! components are rejected before any filesystem access, percent-encoded
//! traversal is decoded first, absolute paths need a workspace grant, and a
//! symlink inside the home that points outside is refused after canonicalize.

use agentos_tools::{AgentTool, FileReader, ToolExecutionContext};
use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
use std::path::Path;

fn ctx(data_dir: &Path, agent_id: AgentID) -> ToolExecutionContext {
    ToolExecutionContext {
        data_dir: data_dir.to_path_buf(),
        task_id: TaskID::new(),
        agent_id,
        trace_id: TraceID::new(),
        permissions: PermissionSet::new(),
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
        shared_dir: None,
    }
}

#[tokio::test]
async fn traversal_and_symlink_escapes_are_rejected_but_home_reads_work() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let agent_id = AgentID::new();
    let context = ctx(&data_dir, agent_id);
    let home = context.agent_files_dir().unwrap();

    std::fs::write(home.join("ok.txt"), "hello from home").unwrap();
    let outside = tmp.path().join("secret.txt");
    std::fs::write(&outside, "CANARY-7f3a-must-never-leak").unwrap();
    std::os::unix::fs::symlink(&outside, home.join("link.txt")).unwrap();

    let tool = FileReader::new();
    let outside_str = outside.to_str().unwrap().to_string();
    let escapes = [
        "../secret.txt",
        "foo/../../secret.txt",
        "%2e%2e/secret.txt",
        "..%2fsecret.txt",
        "/etc/passwd",
        outside_str.as_str(),
        "link.txt",
    ];
    for path in escapes {
        let result = tool
            .execute(
                serde_json::json!({ "path": path }),
                ctx(&data_dir, agent_id),
            )
            .await;
        let err = match result {
            Err(e) => e.to_string(),
            Ok(v) => panic!("escape must be rejected: {path} -> {v}"),
        };
        assert!(
            !err.contains("CANARY-7f3a"),
            "error text must not leak file contents for {path}: {err}"
        );
    }

    // Positive control: a file inside the agent home reads normally.
    let ok = tool
        .execute(
            serde_json::json!({ "path": "ok.txt" }),
            ctx(&data_dir, agent_id),
        )
        .await
        .expect("in-home read must succeed");
    assert!(ok.to_string().contains("hello from home"), "got {ok}");
}
