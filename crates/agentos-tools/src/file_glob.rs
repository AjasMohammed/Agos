use crate::traits::{
    is_cycle_prone, AgentTool, MultiGlob, ToolExecutionContext, CHECK_EVERY, MAX_DEPTH,
    MAX_ENTRIES_SCANNED, MAX_MATCHES, MAX_WALK_SECS,
};
use agentos_types::*;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

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

        // SECURITY: relative paths resolve under the agent's own home, never the
        // kernel state dir (audit.db, api_keys.db, chat.db, agents.json live there).
        let agent_root = context.agent_files_dir()?;
        let base_resolved =
            crate::traits::resolve_tool_path(sub_path, &agent_root, &context.workspace_paths)?;

        let canonical_agent_root =
            agent_root
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
        // KMC Phase 3: check dynamic storage zones
        let in_storage_zone = context
            .storage_zone_query
            .as_ref()
            .map(|q| q.is_path_in_zone(&context.agent_id, &canonical_base))
            .unwrap_or(false);
        if !canonical_base.starts_with(&canonical_agent_root) && !in_workspace && !in_storage_zone {
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

        // Bounded walk from the validated base. `spawn_blocking` tasks cannot be
        // cancelled by dropping the future, so the walk must terminate on its own:
        // the caps below are the only thing standing between a bad pattern and a
        // permanently pinned core.
        let cancel = context.cancellation_token.clone();
        let base_for_walk = canonical_base.clone();
        let pattern_clone = pattern.clone();
        let (matches, stopped_by) = tokio::task::spawn_blocking(move || {
            collect_glob_matches(&base_for_walk, &pattern, &cancel)
        })
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "file-glob".into(),
            reason: format!("Glob task failed: {}", e),
        })??;

        // Build relative paths. Matches under the agent's own home are reported
        // relative to it; anything else (workspace grant / storage zone) keeps
        // its absolute path so the agent can feed it back to other file tools.
        let mut entries: Vec<serde_json::Value> = matches
            .into_iter()
            .map(|(path, meta)| {
                let rel = path
                    .strip_prefix(&canonical_agent_root)
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
        let mut out = serde_json::json!({
            "pattern": pattern_clone,
            "path": sub_path,
            "matches": entries,
            "count": count,
        });
        if let Some(reason) = stopped_by {
            out["truncated"] = serde_json::Value::Bool(true);
            out["note"] = serde_json::Value::String(format!(
                "Search stopped early — hit the {} ({} matches / {} entries scanned / {}s / \
                 depth {}). Results are partial. Narrow it with a more specific 'path' or \
                 pattern rather than repeating this search.",
                reason, MAX_MATCHES, MAX_ENTRIES_SCANNED, MAX_WALK_SECS, MAX_DEPTH
            ));
        }
        Ok(out)
    }
}

/// Matches plus the cap that ended the walk early (`None` = tree exhausted).
type GlobMatches = (Vec<(PathBuf, FileMeta)>, Option<&'static str>);

struct FileMeta {
    size_bytes: u64,
    modified_at: i64, // Unix timestamp seconds
    is_dir: bool,
}

/// Walk `base` and return entries whose path relative to `base` matches `pattern`.
///
/// Returns `(matches, stopped_by)`. `stopped_by` names the cap that ended the
/// walk early, or is `None` when the tree was walked to exhaustion.
///
/// SECURITY / LIVENESS notes:
/// - `follow_links(false)`: symlink farms (pnpm `node_modules`, `.venv/lib64`)
///   form cycles that a link-following walker recurses forever. This is what
///   wedged three blocking threads at 100% CPU for hours before the caps existed.
/// - Every yielded path is still canonicalized and re-checked against `base`, so
///   a symlink *file* pointing outside the granted tree is dropped.
/// - `min_depth(1)` is what keeps the base itself out of `filter_entry`, which
///   is why naming a skipped directory as `path` still searches inside it —
///   walkdir never passes depth-excluded entries to the predicate at all.
fn collect_glob_matches(
    base: &Path,
    pattern: &str,
    cancel: &CancellationToken,
) -> Result<GlobMatches, AgentOSError> {
    let options = glob::MatchOptions {
        case_sensitive: true,
        // MUST stay true. The old code called `glob_with`, which splits the
        // pattern on separators and matches one component at a time, so `*`
        // structurally could not cross a `/` no matter what this flag said
        // (glob 0.3 documents that it forces this to true). `matches_path_with`
        // is the flat matcher and *honours* the flag — leaving it false turns
        // `*.log` from a single-level listing into a depth-12 recursive search
        // of the exact host trees this fix exists to survive. `**` is
        // unaffected: `AnyRecursiveSequence` is not gated on this flag.
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    let matcher = MultiGlob::new(pattern)
        .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid glob pattern: {}", e)))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(MAX_WALK_SECS);
    let mut results = Vec::new();
    let mut scanned = 0usize;
    // Matches are keyed by their *resolved* path, so a symlink and its target
    // both inside the tree do not produce two identical entries.
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    // Which cap stopped the walk, if any. `None` = the tree was exhausted, so a
    // result of exactly MAX_MATCHES is reported as complete rather than truncated.
    let mut stopped_by: Option<&'static str> = None;

    let walker = walkdir::WalkDir::new(base)
        .follow_links(false)
        .max_depth(MAX_DEPTH)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| !is_cycle_prone(e));

    for entry in walker {
        scanned += 1;
        if scanned.is_multiple_of(CHECK_EVERY) {
            if cancel.is_cancelled() {
                stopped_by = Some("cancelled");
                break;
            }
            if std::time::Instant::now() >= deadline {
                stopped_by = Some("time limit");
                break;
            }
        }
        if scanned > MAX_ENTRIES_SCANNED {
            stopped_by = Some("scan limit");
            break;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // unreadable dir / broken link — skip, keep walking
        };

        let rel = match entry.path().strip_prefix(base) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !matcher.matches_path_with(rel, options) {
            continue;
        }

        // Re-verify containment after resolving symlinks.
        let canonical_path = match entry.path().canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if !canonical_path.starts_with(base) {
            continue;
        }
        if !seen.insert(canonical_path.clone()) {
            continue;
        }

        // The reported path is the resolved target, so the metadata must be the
        // target's too — `entry.metadata()` under `follow_links(false)` is
        // `symlink_metadata`, which would report a dir symlink as a ~30-byte file.
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

        if results.len() >= MAX_MATCHES {
            stopped_by = Some("match limit");
            break;
        }
    }

    Ok((results, stopped_by))
}
