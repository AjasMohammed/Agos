use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

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

        // SECURITY: relative paths resolve under the agent's own home, never the
        // kernel state dir (audit.db, api_keys.db, chat.db, agents.json live there).
        let agent_root = context.agent_files_dir()?;
        // SECURITY: resolve search root, checking workspace paths before falling back to data_dir.
        let resolved =
            crate::traits::resolve_tool_path(&search_path, &agent_root, &context.read_roots())
                .map_err(|e| context.with_path_hint(e))?;

        let canonical_root =
            resolved
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-grep".into(),
                    reason: format!("Search path not found: {} ({})", search_path, e),
                })?;

        let canonical_agent_root =
            agent_root
                .canonicalize()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-grep".into(),
                    reason: format!("Data directory error: {}", e),
                })?;

        let in_workspace = context
            .workspace_paths
            .iter()
            .any(|wp| canonical_root.starts_with(wp));
        // KMC Phase 3: check dynamic storage zones
        let in_storage_zone = context
            .storage_zone_query
            .as_ref()
            .map(|q| q.is_path_in_zone(&context.agent_id, &canonical_root))
            .unwrap_or(false);
        if !canonical_root.starts_with(&canonical_agent_root) && !in_workspace && !in_storage_zone {
            return Err(context.deny_path(&search_path));
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

        // Build the filename glob once (brace alternates supported).
        let glob_pattern = glob_filter
            .as_deref()
            .map(|g| {
                crate::traits::MultiGlob::new(g).map_err(|e| {
                    AgentOSError::SchemaValidation(format!(
                        "file-grep: invalid glob filter '{}': {}",
                        g, e
                    ))
                })
            })
            .transpose()?;

        // Allowed roots for this execution: data_dir + any workspace paths.
        let allowed_roots: Vec<PathBuf> = std::iter::once(canonical_agent_root.clone())
            .chain(context.workspace_paths.iter().cloned())
            .collect();

        // Run the search synchronously in a blocking task. NOTE: a blocking task
        // cannot be cancelled by dropping this future, so everything inside must
        // terminate on its own — see the deadline threaded through `search_files`.
        let cancel = context.cancellation_token.clone();
        let results = tokio::task::spawn_blocking(move || {
            search_files(
                &canonical_root,
                &canonical_agent_root,
                &allowed_roots,
                &regex,
                glob_pattern.as_ref(),
                context_lines,
                &output_mode,
                max_results,
                &cancel,
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
    glob_filter: Option<&crate::traits::MultiGlob>,
    context_lines: usize,
    output_mode: &str,
    max_results: usize,
    cancel: &CancellationToken,
) -> Result<serde_json::Value, AgentOSError> {
    // One deadline for the whole tool: walking and searching share the budget, so
    // a walk that stops politely at 20s cannot hand 10 000 files to an unbounded
    // read/regex phase that then pins the thread for minutes.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(crate::traits::MAX_WALK_SECS);
    let (files, mut stopped_by) = collect_files(
        root,
        allowed_roots,
        glob_filter,
        MAX_FILES_TO_SEARCH,
        deadline,
    );

    let mut matches: Vec<serde_json::Value> = Vec::new();
    let mut files_with_matches: Vec<String> = Vec::new();
    let mut total_match_count: usize = 0;
    let mut files_searched: usize = 0;

    'file_loop: for file_path in &files {
        // Reading and scanning is the expensive half; `count` mode has no early
        // exit at all, so without this the loop always runs every collected file.
        if cancel.is_cancelled() {
            stopped_by = stopped_by.or(Some("cancelled"));
            break;
        }
        if std::time::Instant::now() >= deadline {
            stopped_by = stopped_by.or(Some("time limit"));
            break;
        }
        files_searched += 1;

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
            "files_searched": files_searched,
        }),
        _ => unreachable!(),
    };

    let mut result = result;
    if let Some(reason) = stopped_by {
        result["truncated"] = serde_json::Value::Bool(true);
        result["note"] = serde_json::Value::String(format!(
            "Search stopped early — hit the {} ({} files / {} entries scanned / {}s / depth {}). \
             Results are partial. Narrow it with a more specific 'path' or 'glob' rather than \
             repeating this search.",
            reason,
            MAX_FILES_TO_SEARCH,
            crate::traits::MAX_ENTRIES_SCANNED,
            crate::traits::MAX_WALK_SECS,
            crate::traits::MAX_DEPTH
        ));
    }

    Ok(result)
}

/// Collect files under `root` to search.
///
/// LIVENESS: the previous BFS canonicalized every entry and re-queued it, so a
/// symlink that resolves back to an ancestor (pnpm `node_modules`, `.venv/lib64`)
/// re-enqueued the same directory forever. `max_files` did not save it — when the
/// glob filter matched nothing, `result` never grew and the loop never ended.
/// The walk now refuses to follow links and is capped on depth, entries and time.
fn collect_files(
    root: &Path,
    allowed_roots: &[PathBuf],
    glob_filter: Option<&crate::traits::MultiGlob>,
    max_files: usize,
    deadline: std::time::Instant,
) -> (Vec<PathBuf>, Option<&'static str>) {
    let mut result = Vec::new();
    let mut scanned = 0usize;
    let mut stopped_by: Option<&'static str> = None;
    // Entries are keyed by their *resolved* path, so a symlink and its target
    // both living inside the tree would otherwise be searched (and reported)
    // twice — `~/.bashrc -> ~/dotfiles/bashrc` is the common shape.
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(crate::traits::MAX_DEPTH)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| !crate::traits::is_cycle_prone(e));

    for entry in walker {
        scanned += 1;
        if scanned > crate::traits::MAX_ENTRIES_SCANNED {
            stopped_by = Some("scan limit");
            break;
        }
        if scanned.is_multiple_of(crate::traits::CHECK_EVERY)
            && std::time::Instant::now() >= deadline
        {
            stopped_by = Some("time limit");
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        // Directories are the walker's business, not ours. Anything else — a
        // regular file OR a symlink to one — is a search candidate; under
        // `follow_links(false)` a symlinked file reports as `is_symlink()`, not
        // `is_file()`, so testing `is_file()` here would silently hide the whole
        // dotfiles layout (`~/.bashrc -> ~/dotfiles/bashrc`).
        if entry.file_type().is_dir() {
            continue;
        }

        // SECURITY: verify every entry stays within an allowed root after
        // resolving symlinks. Following the link is safe *here* because
        // containment is re-checked against the resolved path.
        let Ok(canonical_path) = entry.path().canonicalize() else {
            continue;
        };
        if !allowed_roots
            .iter()
            .any(|root| canonical_path.starts_with(root))
        {
            continue;
        }
        if !canonical_path.is_file() {
            continue;
        }
        if !seen.insert(canonical_path.clone()) {
            continue;
        }

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
            stopped_by = Some("file limit");
            break;
        }
    }

    (result, stopped_by)
}
