use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

pub struct FileGlob;

impl FileGlob {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileGlob {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for FileGlob {
    fn name(&self) -> &str {
        "file-glob"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let pattern = payload
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("file-glob requires 'pattern' field".into())
            })?
            .to_string();

        // SECURITY: reject patterns that could escape any directory.
        if pattern.contains("..") {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: "Glob pattern must not contain '..'".into(),
            });
        }
        if pattern.starts_with('/') {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: "Glob pattern must not be absolute".into(),
            });
        }

        // Optional sub-directory within data_dir or a configured workspace path.
        let sub_path = payload.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        // Reject traversal in path parameter (defense-in-depth before canonicalize).
        if sub_path.contains("..") {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: "Path must not contain '..'".into(),
            });
        }

        let base_resolved =
            crate::traits::resolve_tool_path(sub_path, &context.data_dir, &context.workspace_paths);

        let canonical_data_dir =
            context
                .data_dir
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-glob".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        let canonical_base =
            base_resolved
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-glob".into(),
                    reason: format!("Base path not found: {} ({})", sub_path, e),
                })?;

        let in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| canonical_base.starts_with(wp));
        if !canonical_base.starts_with(&canonical_data_dir) && !in_workspace {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied: {}", sub_path),
            });
        }
        if in_workspace
            && !context
                .permissions
                .check("fs.workspace", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.workspace".into(),
                operation: format!("Workspace read access denied: {}", sub_path),
            });
        }

        // Build the full glob string: base/pattern
        let full_pattern = format!("{}/{}", canonical_base.display(), pattern);
        let pattern_clone = pattern.clone();
        // Allowed roots for this execution: data_dir + any workspace paths.
        let allowed_roots: Vec<PathBuf> = std::iter::once(canonical_data_dir.clone())
            .chain(context.workspace_paths.iter().cloned())
            .collect();

        let (matches, canonical_data_dir_clone) = tokio::task::spawn_blocking(move || {
            collect_glob_matches(&full_pattern, &canonical_data_dir, &allowed_roots)
        })
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "file-glob".into(),
            reason: format!("Glob task failed: {}", e),
        })??;

        // Build relative paths. For workspace matches, use absolute path; for data_dir matches
        // strip the data_dir prefix to produce a relative path as before.
        let mut entries: Vec<serde_json::Value> = matches
            .into_iter()
            .map(|(path, meta)| {
                let rel = path
                    .strip_prefix(&canonical_data_dir_clone)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| path.to_string_lossy().to_string());
                serde_json::json!({
                    "path": rel,
                    "size_bytes": meta.size_bytes,
                    "modified_at": meta.modified_at,
                    "is_dir": meta.is_dir,
                })
            })
            .collect();

        // Sort by modified_at descending (most recent first).
        entries.sort_by(|a, b| {
            b.get("modified_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .cmp(&a.get("modified_at").and_then(|v| v.as_i64()).unwrap_or(0))
        });

        let count = entries.len();
        Ok(serde_json::json!({
            "pattern": pattern_clone,
            "path": sub_path,
            "matches": entries,
            "count": count,
        }))
    }
}

struct FileMeta {
    size_bytes: u64,
    modified_at: i64, // Unix timestamp seconds
    is_dir: bool,
}

fn collect_glob_matches(
    full_pattern: &str,
    canonical_data_dir: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(Vec<(PathBuf, FileMeta)>, PathBuf), AgentOSError> {
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    let paths = glob::glob_with(full_pattern, options)
        .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid glob pattern: {}", e)))?;

    let mut results = Vec::new();
    for entry in paths {
        let path = match entry {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Canonicalize each result and verify it stays within an allowed root.
        let canonical_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let allowed = allowed_roots
            .iter()
            .any(|root| canonical_path.starts_with(root));
        if !allowed {
            continue;
        }

        let meta = std::fs::metadata(&canonical_path).ok();
        let file_meta = FileMeta {
            size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified_at: meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs() as i64)
                })
                .unwrap_or(0),
            is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
        };
        results.push((canonical_path, file_meta));
    }

    Ok((results, canonical_data_dir.to_path_buf()))
}
