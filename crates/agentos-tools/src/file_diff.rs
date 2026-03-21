use crate::traits::{resolve_tool_path, AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use similar::{ChangeTag, TextDiff};
use std::fmt::Write as FmtWrite;

pub struct FileDiff;

impl FileDiff {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileDiff {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for FileDiff {
    fn name(&self) -> &str {
        "file-diff"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("fs.user_data", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".to_string(),
                operation: "Read".to_string(),
            });
        }

        let mode = payload
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("files");

        let (text_a, text_b, label_a, label_b) = match mode {
            "strings" => {
                let a = payload
                    .get("text_a")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "file-diff mode=strings requires 'text_a'".into(),
                        )
                    })?
                    .to_string();
                let b = payload
                    .get("text_b")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "file-diff mode=strings requires 'text_b'".into(),
                        )
                    })?
                    .to_string();
                (a, b, "a".to_string(), "b".to_string())
            }
            _ => {
                // mode = "files" (default)
                let path_a = payload
                    .get("file_a")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation("file-diff requires 'file_a'".into())
                    })?;
                let path_b = payload
                    .get("file_b")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation("file-diff requires 'file_b'".into())
                    })?;

                // Defense-in-depth: explicit traversal check before canonicalization.
                for p in [path_a, path_b] {
                    if p.contains("..") {
                        return Err(AgentOSError::PermissionDenied {
                            resource: p.to_string(),
                            operation: "Read (path traversal blocked)".to_string(),
                        });
                    }
                }

                let data_dir_canon = context.data_dir.canonicalize().map_err(|_| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "file-diff".into(),
                        reason: "Cannot resolve data directory".into(),
                    }
                })?;

                let resolve = |p: &str| -> Result<std::path::PathBuf, AgentOSError> {
                    let resolved =
                        resolve_tool_path(p, &context.data_dir, &context.workspace_paths);
                    resolved
                        .canonicalize()
                        .map_err(|_| AgentOSError::ToolExecutionFailed {
                            tool_name: "file-diff".into(),
                            reason: format!("File not found: {}", p),
                        })
                };

                let canon_a = resolve(path_a)?;
                let canon_b = resolve(path_b)?;

                for (path_str, canon) in [(path_a, &canon_a), (path_b, &canon_b)] {
                    let in_workspace = context
                        .workspace_paths
                        .iter()
                        .any(|wp| canon.starts_with(wp));
                    if !canon.starts_with(&data_dir_canon) && !in_workspace {
                        return Err(AgentOSError::PermissionDenied {
                            resource: "fs.user_data".into(),
                            operation: format!("Path traversal denied: {}", path_str),
                        });
                    }
                    if in_workspace
                        && !context
                            .permissions
                            .check("fs.workspace", PermissionOp::Read)
                    {
                        return Err(AgentOSError::PermissionDenied {
                            resource: "fs.workspace".into(),
                            operation: format!("Workspace read access denied: {}", path_str),
                        });
                    }
                }

                let a = tokio::fs::read_to_string(&canon_a).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "file-diff".into(),
                        reason: format!("Read failed for file_a: {}", e),
                    }
                })?;
                let b = tokio::fs::read_to_string(&canon_b).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "file-diff".into(),
                        reason: format!("Read failed for file_b: {}", e),
                    }
                })?;

                (a, b, path_a.to_string(), path_b.to_string())
            }
        };

        let context_lines = payload
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as usize;

        let diff = TextDiff::from_lines(&text_a, &text_b);
        let mut unified = String::new();
        let _ = writeln!(unified, "--- {}", label_a);
        let _ = writeln!(unified, "+++ {}", label_b);

        for group in diff.grouped_ops(context_lines) {
            for op in &group {
                for change in diff.iter_changes(op) {
                    let prefix = match change.tag() {
                        ChangeTag::Delete => "-",
                        ChangeTag::Insert => "+",
                        ChangeTag::Equal => " ",
                    };
                    let _ = write!(unified, "{}{}", prefix, change.value());
                    if change.missing_newline() {
                        let _ = writeln!(unified);
                    }
                }
            }
        }

        let is_identical = diff.ratio() >= 1.0;

        Ok(serde_json::json!({
            "label_a": label_a,
            "label_b": label_b,
            "identical": is_identical,
            "diff": unified,
            "similarity_ratio": diff.ratio(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;

    fn ctx() -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant("fs.user_data".to_string(), true, false, false, None);
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions,
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn diff_identical_strings() {
        let tool = FileDiff::new();
        let result = tool
            .execute(
                serde_json::json!({
                    "mode": "strings",
                    "text_a": "hello\nworld\n",
                    "text_b": "hello\nworld\n",
                }),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(result["identical"], true);
    }

    #[tokio::test]
    async fn diff_changed_strings_produces_output() {
        let tool = FileDiff::new();
        let result = tool
            .execute(
                serde_json::json!({
                    "mode": "strings",
                    "text_a": "hello\nworld\n",
                    "text_b": "hello\nearth\n",
                }),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(result["identical"], false);
        let diff_str = result["diff"].as_str().unwrap();
        assert!(diff_str.contains("-world"));
        assert!(diff_str.contains("+earth"));
    }

    #[tokio::test]
    async fn diff_rejects_path_traversal() {
        let tool = FileDiff::new();
        let result = tool
            .execute(
                serde_json::json!({
                    "mode": "files",
                    "file_a": "../secret",
                    "file_b": "normal.txt",
                }),
                ctx(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[tokio::test]
    async fn diff_requires_permission() {
        let tool = FileDiff::new();
        let ctx_no_perm = ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        };
        let result = tool
            .execute(
                serde_json::json!({"mode": "strings", "text_a": "a", "text_b": "b"}),
                ctx_no_perm,
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }
}
