use agentos_tools::shell_exec::ShellExec;
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
use std::path::Path;
use tempfile::TempDir;

fn make_context(data_dir: &Path) -> ToolExecutionContext {
    ToolExecutionContext {
        data_dir: data_dir.to_path_buf(),
        task_id: TaskID::new(),
        agent_id: AgentID::new(),
        trace_id: TraceID::new(),
        permissions: PermissionSet::new(),
        vault: None,
        hal: None,
    }
}

#[tokio::test]
async fn test_shell_exec_bwrap_root() {
    let dir = TempDir::new().unwrap();
    let tool = ShellExec::new();

    // Check if bwrap exists, otherwise this test is meaningless
    if std::process::Command::new("bwrap")
        .arg("--version")
        .output()
        .is_err()
    {
        println!("Skipping bwrap test because bwrap is not installed");
        return;
    }

    let result = tool
        .execute(
            serde_json::json!({"command": "ls /root"}),
            make_context(dir.path()),
        )
        .await
        .unwrap();

    let stderr = result["stderr"].as_str().unwrap();
    assert!(
        stderr.contains("No such file or directory")
            || stderr.contains("Permission denied")
            || result["stdout"].as_str().unwrap().is_empty(),
        "Should not be able to list /root. Got stderr: {}, stdout: {}",
        stderr,
        result["stdout"]
    );
}

#[tokio::test]
async fn test_shell_exec_bwrap_etc() {
    let dir = TempDir::new().unwrap();
    let tool = ShellExec::new();

    if std::process::Command::new("bwrap")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }

    let result = tool
        .execute(
            serde_json::json!({"command": "cat /etc/shadow"}),
            make_context(dir.path()),
        )
        .await
        .unwrap();

    let stderr = result["stderr"].as_str().unwrap();
    assert!(
        stderr.contains("No such file or directory") || stderr.contains("Permission denied"),
        "Should not be able to read /etc/shadow. Got stderr: {}",
        stderr
    );
}
