use agentos_types::*;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Every tool implements this trait.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// The tool's name (must match manifest).
    fn name(&self) -> &str;

    /// Execute the tool with the given payload.
    /// The kernel has already validated the capability token and permissions
    /// before calling this method.
    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError>;

    /// Return the permissions this tool requires to operate.
    fn required_permissions(&self) -> Vec<(String, PermissionOp)>;

    /// Return permissions required for this specific payload.
    ///
    /// Default behavior preserves legacy tools that declare a static
    /// permission set across all actions.
    fn required_permissions_for(
        &self,
        _payload: &serde_json::Value,
    ) -> Vec<(String, PermissionOp)> {
        self.required_permissions()
    }
}

/// Context provided to the tool at execution time.
/// Contains references to kernel resources the tool is allowed to use.
#[derive(Clone)]
pub struct ToolExecutionContext {
    pub data_dir: PathBuf, // /opt/agentos/data — where tools read/write files
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub trace_id: TraceID,
    pub permissions: PermissionSet,
    pub vault: Option<std::sync::Arc<agentos_vault::ProxyVault>>,
    pub hal: Option<std::sync::Arc<agentos_hal::HardwareAbstractionLayer>>,
    /// Shared file lock registry injected by `ToolRunner`. `None` when tools
    /// are called directly in tests without going through the runner.
    pub file_lock_registry: Option<std::sync::Arc<crate::file_lock::FileLockRegistry>>,
    /// Snapshot of the agent registry at task dispatch time. `None` outside kernel context.
    pub agent_registry: Option<std::sync::Arc<dyn AgentRegistryQuery>>,
    /// Snapshot of the task store at task dispatch time. `None` outside kernel context.
    pub task_registry: Option<std::sync::Arc<dyn TaskQuery>>,
    /// Snapshot of the escalation manager at task dispatch time. `None` outside kernel context.
    pub escalation_query: Option<std::sync::Arc<dyn EscalationQuery>>,
    /// Additional directories the agent may access beyond `data_dir`.
    /// Populated from `tools.workspace.allowed_paths` in the kernel config.
    /// Paths are pre-canonicalized at kernel startup.
    pub workspace_paths: Vec<PathBuf>,
    /// Cancellation token for this tool invocation. Tools that perform
    /// long-running I/O (HTTP, shell exec) should check this token periodically
    /// and return early with a `ToolExecutionFailed` error if it is cancelled.
    pub cancellation_token: CancellationToken,
}

/// Percent-decode ASCII bytes in a path string (e.g. `%2e%2e` → `..`, `%2f` → `/`).
///
/// Only decodes sequences that produce ASCII bytes (0x00–0x7F). Sequences that
/// would produce bytes 0x80–0xFF are left as literal `%xx` text. This avoids
/// ambiguity around multi-byte UTF-8 sequences (e.g. overlong encodings like
/// `%C0%AE` for `.`) while still catching the common ASCII-encoded traversal
/// patterns (`%2e%2e`, `%2F`, etc.) that `contains_traversal` then rejects.
fn percent_decode_path(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                let decoded = hi << 4 | lo;
                // Only expand ASCII bytes (0x00-0x7F). Non-ASCII percent sequences
                // are kept as-is; they cannot produce a `..` traversal component.
                if decoded < 0x80 {
                    out.push(decoded as char);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Returns `true` if any component of the path is `..` (parent traversal).
fn contains_traversal(path: &Path) -> bool {
    use std::path::Component;
    path.components().any(|c| matches!(c, Component::ParentDir))
}

/// Resolve a user-supplied path for file tools, respecting workspace paths.
///
/// Resolution rules:
/// - The input is first percent-decoded (`%2e%2e` → `..`, `%2f` → `/`) and
///   then explicitly rejected if any component is `..` (defence-in-depth on
///   top of the canonicalize check the caller must still perform).
/// - Relative path → joined onto `data_dir`.
/// - Absolute path that starts with a configured workspace prefix → used as-is.
/// - Absolute path with no workspace match → the leading `/` is stripped and the
///   remainder is joined onto `data_dir` (legacy behavior for data-dir-relative
///   absolute paths).
///
/// The caller must still canonicalize the result and verify containment within
/// `data_dir` or one of `workspace_paths`.
pub fn resolve_tool_path(
    path_str: &str,
    data_dir: &Path,
    workspace_paths: &[PathBuf],
) -> Result<PathBuf, agentos_types::AgentOSError> {
    // SECURITY: percent-decode first to catch %2e%2e (%2F, etc.)
    let decoded = percent_decode_path(path_str);

    let p = Path::new(&decoded);

    // SECURITY: explicitly reject `..` components (belt-and-suspenders with canonicalize)
    if contains_traversal(p) {
        return Err(agentos_types::AgentOSError::PermissionDenied {
            resource: "fs.user_data".into(),
            operation: format!(
                "Path traversal denied: path contains '..' component: {}",
                path_str
            ),
        });
    }

    let resolved = if p.is_absolute() {
        // If this absolute path is within a configured workspace, use it directly.
        let mut matched = None;
        for wp in workspace_paths {
            if p.starts_with(wp) {
                matched = Some(p.to_path_buf());
                break;
            }
        }
        matched.unwrap_or_else(|| {
            // Fall back: strip the leading `/` and resolve relative to data_dir.
            let stripped = p.strip_prefix("/").unwrap_or(p);
            data_dir.join(stripped)
        })
    } else {
        data_dir.join(p)
    };

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn data_dir() -> PathBuf {
        PathBuf::from("/opt/agentos/data")
    }

    #[test]
    fn resolve_relative_path() {
        let r = resolve_tool_path("notes/file.txt", &data_dir(), &[]).unwrap();
        assert_eq!(r, PathBuf::from("/opt/agentos/data/notes/file.txt"));
    }

    #[test]
    fn resolve_absolute_workspace_path() {
        let ws = vec![PathBuf::from("/home/user/project")];
        let r = resolve_tool_path("/home/user/project/src/main.rs", &data_dir(), &ws).unwrap();
        assert_eq!(r, PathBuf::from("/home/user/project/src/main.rs"));
    }

    #[test]
    fn resolve_absolute_non_workspace_strips_slash() {
        let r = resolve_tool_path("/etc/passwd", &data_dir(), &[]).unwrap();
        assert_eq!(r, PathBuf::from("/opt/agentos/data/etc/passwd"));
    }

    #[test]
    fn reject_dotdot_traversal() {
        let err = resolve_tool_path("../../../etc/passwd", &data_dir(), &[]);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("Path traversal denied"));
    }

    #[test]
    fn reject_url_encoded_dotdot() {
        // %2e = '.', so %2e%2e = '..'
        let err = resolve_tool_path("%2e%2e/%2e%2e/etc/passwd", &data_dir(), &[]);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("Path traversal denied"));
    }

    #[test]
    fn reject_mixed_case_url_encoded_dotdot() {
        // %2E = '.', uppercase hex
        let err = resolve_tool_path("%2E%2E/secret", &data_dir(), &[]);
        assert!(err.is_err());
    }

    #[test]
    fn reject_url_encoded_slash_traversal() {
        // %2f = '/', so dir%2f..%2f.. = dir/../..
        let err = resolve_tool_path("dir%2f..%2f../etc/passwd", &data_dir(), &[]);
        assert!(err.is_err());
    }

    #[test]
    fn reject_double_encoded_is_safe() {
        // %252e = literal '%2e' after single decode — should NOT decode further
        // This results in a path component '%2e%2e' which is a literal name, not '..'
        let r = resolve_tool_path("%252e%252e/file.txt", &data_dir(), &[]);
        assert!(r.is_ok()); // single decode yields '%2e%2e', which is a literal name
    }

    #[test]
    fn percent_decode_normal_path_unchanged() {
        assert_eq!(percent_decode_path("hello/world.txt"), "hello/world.txt");
    }

    #[test]
    fn percent_decode_encoded_dots() {
        assert_eq!(percent_decode_path("%2e%2e"), "..");
    }

    #[test]
    fn percent_decode_partial_sequence_passes_through() {
        // Incomplete percent sequence at end
        assert_eq!(percent_decode_path("hello%2"), "hello%2");
        assert_eq!(percent_decode_path("hello%"), "hello%");
    }

    #[test]
    fn percent_decode_non_ascii_bytes_left_as_literal() {
        // %C0%AE is an overlong UTF-8 encoding for '.' — must NOT be decoded to avoid
        // confusion. The literal %C0%AE string cannot form a `..` traversal component.
        assert_eq!(percent_decode_path("%C0%AE"), "%C0%AE");
        assert_eq!(percent_decode_path("%c0%ae"), "%c0%ae");
        // %80 is also left as-is
        assert_eq!(percent_decode_path("%80"), "%80");
    }

    #[test]
    fn overlong_encoding_attack_safely_rejected() {
        // %C0%AE%C0%AE = overlong encoding of '..' — must not traverse
        // After non-ASCII passthrough, the path is "%C0%AE%C0%AE" which Path::new
        // treats as a non-traversal filename, so resolve_tool_path returns Ok.
        let r = resolve_tool_path("%C0%AE%C0%AE/etc/passwd", &data_dir(), &[]);
        // Either it's blocked by traversal detection, or the literal path doesn't
        // contain a ParentDir component and is just a weird filename — both are safe.
        match r {
            Ok(p) => {
                // If allowed, the resulting path must still be under data_dir
                // (the canonicalize check in the caller enforces this at runtime)
                assert!(!p.to_string_lossy().contains(".."));
            }
            Err(_) => {} // blocked is also fine
        }
    }

    #[test]
    fn contains_traversal_detects_dotdot() {
        assert!(contains_traversal(Path::new("a/../b")));
        assert!(contains_traversal(Path::new("../")));
        assert!(contains_traversal(Path::new("a/b/../../c")));
    }

    #[test]
    fn contains_traversal_allows_normal_paths() {
        assert!(!contains_traversal(Path::new("a/b/c")));
        assert!(!contains_traversal(Path::new("..."))); // three dots is not traversal
        assert!(!contains_traversal(Path::new("a/..hidden/b")));
    }
}
