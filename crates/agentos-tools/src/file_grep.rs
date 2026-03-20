use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_RESULTS: usize = 50;
const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB
/// MED-1: File traversal cap is separate from (and larger than) max_results.
/// max_results limits matches/files returned; MAX_FILES_TO_SEARCH limits traversal cost.
const MAX_FILES_TO_SEARCH: usize = 10_000;

pub struct FileGrep;

impl FileGrep {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileGrep {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for FileGrep {
    fn name(&self) -> &str {
        "file-grep"
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
                AgentOSError::SchemaValidation("file-grep requires 'pattern' field".into())
            })?
            .to_string();

        let search_path = payload
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();

        let glob_filter = payload
            .get("glob")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let context_lines = payload
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let output_mode = payload
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("files_with_matches")
            .to_string();

        let max_results = payload
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_MAX_RESULTS as u64) as usize;

        let case_insensitive = payload
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match output_mode.as_str() {
            "content" | "files_with_matches" | "count" => {}
            other => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "file-grep: unknown output_mode '{}'; expected content | files_with_matches | count",
                    other
                )));
            }
        }

        // Validate and compile the regex.
        let regex = regex::RegexBuilder::new(&pattern)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| {
                AgentOSError::SchemaValidation(format!("file-grep: invalid regex: {}", e))
            })?;

        // SECURITY: resolve search root, checking workspace paths before falling back to data_dir.
        let resolved = crate::traits::resolve_tool_path(
            &search_path,
            &context.data_dir,
            &context.workspace_paths,
        );

        let canonical_root =
            resolved
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-grep".into(),
                    reason: format!("Search path not found: {} ({})", search_path, e),
                })?;

        let canonical_data_dir =
            context
                .data_dir
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-grep".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        let in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| canonical_root.starts_with(wp));
        if !canonical_root.starts_with(&canonical_data_dir) && !in_workspace {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied: {}", search_path),
            });
        }
        if in_workspace
            && !context
                .permissions
                .check("fs.workspace", PermissionOp::Read)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.workspace".into(),
                operation: format!("Workspace read access denied: {}", search_path),
            });
        }

        // Build glob::Pattern for filename filtering once.
        let glob_pattern = glob_filter
            .as_deref()
            .map(|g| {
                glob::Pattern::new(g).map_err(|e| {
                    AgentOSError::SchemaValidation(format!(
                        "file-grep: invalid glob filter '{}': {}",
                        g, e
                    ))
                })
            })
            .transpose()?;

        // Allowed roots for this execution: data_dir + any workspace paths.
        let allowed_roots: Vec<PathBuf> = std::iter::once(canonical_data_dir.clone())
            .chain(context.workspace_paths.iter().cloned())
            .collect();

        // Run the search synchronously in a blocking task.
        let results = tokio::task::spawn_blocking(move || {
            search_files(
                &canonical_root,
                &canonical_data_dir,
                &allowed_roots,
                &regex,
                glob_pattern.as_ref(),
                context_lines,
                &output_mode,
                max_results,
            )
        })
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "file-grep".into(),
            reason: format!("Grep task failed: {}", e),
        })??;

        Ok(results)
    }
}

#[allow(clippy::too_many_arguments)]
fn search_files(
    root: &Path,
    data_dir: &Path,
    allowed_roots: &[PathBuf],
    regex: &regex::Regex,
    glob_filter: Option<&glob::Pattern>,
    context_lines: usize,
    output_mode: &str,
    max_results: usize,
) -> Result<serde_json::Value, AgentOSError> {
    let files = collect_files(root, allowed_roots, glob_filter, MAX_FILES_TO_SEARCH);

    let mut matches: Vec<serde_json::Value> = Vec::new();
    let mut files_with_matches: Vec<String> = Vec::new();
    let mut total_match_count: usize = 0;

    'file_loop: for file_path in &files {
        let meta = std::fs::metadata(file_path).ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_SIZE_BYTES {
            continue;
        }

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue, // skip binary/unreadable files
        };

        let rel_path = file_path
            .strip_prefix(data_dir)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| file_path.to_string_lossy().to_string());

        let lines: Vec<&str> = content.lines().collect();
        let mut file_matched = false;

        for (line_idx, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                file_matched = true;
                total_match_count += 1;

                if output_mode == "content" {
                    let before_start = line_idx.saturating_sub(context_lines);
                    let after_end = (line_idx + context_lines + 1).min(lines.len());

                    let context_before: Vec<&str> = if context_lines > 0 {
                        lines[before_start..line_idx].to_vec()
                    } else {
                        vec![]
                    };
                    let context_after: Vec<&str> = if context_lines > 0 {
                        lines[(line_idx + 1)..after_end].to_vec()
                    } else {
                        vec![]
                    };

                    matches.push(serde_json::json!({
                        "file": rel_path,
                        "line": line_idx + 1,
                        "content": line,
                        "context_before": context_before,
                        "context_after": context_after,
                    }));
                }

                if matches.len() >= max_results && output_mode == "content" {
                    break 'file_loop;
                }
            }
        }

        if file_matched && output_mode == "files_with_matches" {
            files_with_matches.push(rel_path);
            if files_with_matches.len() >= max_results {
                break;
            }
        }
    }

    let result = match output_mode {
        "content" => serde_json::json!({
            "output_mode": "content",
            "matches": matches,
            "count": matches.len(),
        }),
        "files_with_matches" => serde_json::json!({
            "output_mode": "files_with_matches",
            "files": files_with_matches,
            "count": files_with_matches.len(),
        }),
        "count" => serde_json::json!({
            "output_mode": "count",
            "match_count": total_match_count,
            "files_searched": files.len(),
        }),
        _ => unreachable!(),
    };

    Ok(result)
}

fn collect_files(
    root: &Path,
    allowed_roots: &[PathBuf],
    glob_filter: Option<&glob::Pattern>,
    max_files: usize,
) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(root.to_path_buf());

    while let Some(dir) = queue.pop_front() {
        if result.len() >= max_files {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();

            // SECURITY: verify every entry stays within an allowed root.
            let Ok(canonical_path) = path.canonicalize() else {
                continue;
            };
            if !allowed_roots
                .iter()
                .any(|root| canonical_path.starts_with(root))
            {
                continue;
            }

            if canonical_path.is_dir() {
                queue.push_back(canonical_path);
            } else if canonical_path.is_file() {
                if let Some(filter) = glob_filter {
                    let file_name = canonical_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    if !filter.matches(file_name) {
                        continue;
                    }
                }
                result.push(canonical_path);
                if result.len() >= max_files {
                    break;
                }
            }
        }
    }

    result
}
