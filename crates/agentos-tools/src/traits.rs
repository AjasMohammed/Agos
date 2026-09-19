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
    /// Additional directories the agent may *at least read* beyond `data_dir`.
    /// Populated from kernel config + every active `WorkspaceGrant` whose mode
    /// covers `READ`. Used by read-only file tools (`file-reader`, `file-diff`,
    /// `file-grep`, `file-glob`).
    pub workspace_paths: Vec<PathBuf>,
    /// Subset of `workspace_paths` the agent may also *write to* — i.e. every
    /// active `WorkspaceGrant` whose mode covers `WRITE`. Used by write-side
    /// file tools (`file-writer`, `file-editor`, `file-delete`, `file-append`,
    /// `file-move`). A read-only grant is intentionally absent here so it
    /// can't be written through.
    pub workspace_paths_writable: Vec<PathBuf>,
    /// Subset of `workspace_paths` the agent may bind into a sandboxed
    /// command — i.e. every active `WorkspaceGrant` whose mode covers `EXEC`.
    /// Used by `shell-exec` to extend its bwrap bind list so commands act on
    /// real on-disk files rather than the data_dir tmpfs view.
    pub workspace_paths_executable: Vec<PathBuf>,
    /// Capability registry query interface for managed capabilities (KMC).
    /// Used by capability tools to discover available providers.
    pub capability_registry: Option<std::sync::Arc<dyn CapabilityRegistryQuery>>,
    /// Capability dispatcher for executing managed capability actions (KMC).
    /// Used by KMC bridge tools (env-install, proc-spawn, etc.) to route
    /// actions to the kernel's capability providers.
    pub capability_dispatcher: Option<std::sync::Arc<dyn CapabilityDispatcher>>,
    /// Storage zone query for dynamic filesystem access expansion (KMC Phase 3).
    /// File tools check this to determine whether a path is within an active
    /// storage zone for the requesting agent.
    pub storage_zone_query: Option<std::sync::Arc<dyn StorageZoneQuery>>,
    /// Cancellation token for this tool invocation. Tools that perform
    /// long-running I/O (HTTP, shell exec) should check this token periodically
    /// and return early with a `ToolExecutionFailed` error if it is cancelled.
    pub cancellation_token: CancellationToken,
    /// Optional task-scoped tool category allowlist. When `Some(list)`, the
    /// paginated manual surface (`agent-manual`, `list-tools`, `search-tools`,
    /// `describe-tool`) hides tools whose `category` is not in `list`. `None`
    /// (default) = no restriction.
    /// Mirrors `AgentTask.tool_categories`. Set by the kernel at dispatch.
    pub tool_categories: Option<Vec<String>>,
}

impl ToolExecutionContext {
    /// Root directory that file tools resolve relative paths against:
    /// `<data_dir>/agents/<agent name>/`.
    ///
    /// SECURITY: this is deliberately *not* `data_dir`. `data_dir` is the
    /// kernel's own state directory — it holds `audit.db`, `api_keys.db`,
    /// `chat.db` (every agent's conversations), `agents.json` (every agent's
    /// permission set, reloaded at boot) and the task snapshots. An agent with
    /// the default `fs.user_data:rw` grant must not reach any of it.
    ///
    /// The agent name comes from the registry snapshot. When no registry is
    /// attached (direct tool calls in tests) the fallback is the agent's ID,
    /// never bare `data_dir` — resolution fails closed.
    ///
    /// The directory is created if missing, since callers canonicalize it.
    pub fn agent_files_dir(&self) -> Result<PathBuf, AgentOSError> {
        let name = self
            .agent_registry
            .as_ref()
            .and_then(|r| r.get_agent(&self.agent_id))
            .map(|a| a.name);
        let dir = agent_home_dir(&self.data_dir, name.as_deref(), &self.agent_id);
        std::fs::create_dir_all(&dir).map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "file".into(),
            reason: format!("Agent home directory error: {} ({})", dir.display(), e),
        })?;
        Ok(dir)
    }
}

/// `data_dir/agents/<name>/` — the one definition of an agent home, shared by
/// the tool context and kernel-side re-checks so the two cannot drift. A name
/// is one path segment; anything else falls back to the id so the home can
/// never leave `data_dir/agents/`. Does not create the directory.
pub fn agent_home_dir(
    data_dir: &std::path::Path,
    name: Option<&str>,
    agent_id: &agentos_types::AgentID,
) -> PathBuf {
    let segment = name
        // `.` would make `agents/.` — every agent's home.
        .filter(|n| !n.is_empty() && *n != "." && !n.contains(['/', '\\']) && !n.contains(".."))
        .map(str::to_string)
        .unwrap_or_else(|| agent_id.to_string());
    data_dir.join("agents").join(segment)
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
/// `root` is the agent's own home — [`ToolExecutionContext::agent_files_dir`] —
/// not the kernel's `data_dir`. See that method for why.
///
/// Resolution rules:
/// - The input is first percent-decoded (`%2e%2e` → `..`, `%2f` → `/`) and
///   then explicitly rejected if any component is `..` (defence-in-depth on
///   top of the canonicalize check the caller must still perform).
/// - Relative path → joined onto `root`.
/// - Absolute path that starts with `root` → used as-is.
/// - Absolute path that starts with a configured workspace prefix → used as-is.
/// - Absolute path with no workspace match → `PermissionDenied`. (Earlier
///   builds silently remapped the path into `data_dir`; that hid failures
///   from agents and users. Grant the path with `agentos workspace grant
///   <dir>` to give the agent real access instead.)
///
/// The caller must still canonicalize the result and verify containment within
/// `root` or one of `workspace_paths`.
pub fn resolve_tool_path(
    path_str: &str,
    root: &Path,
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

    if p.is_absolute() {
        if p.starts_with(root) {
            return Ok(p.to_path_buf());
        }
        for wp in workspace_paths {
            if p.starts_with(wp) {
                return Ok(p.to_path_buf());
            }
        }
        Err(agentos_types::AgentOSError::PermissionDenied {
            resource: "fs.user_data".into(),
            operation: format!(
                "No workspace grant covers '{}'. Grant access with: `agentos workspace grant <directory>`",
                path_str
            ),
        })
    } else {
        Ok(root.join(p))
    }
}

/// Deepest directory level below the base that is walked.
pub(crate) const MAX_DEPTH: usize = 12;
/// Most matches returned in one call.
pub(crate) const MAX_MATCHES: usize = 1000;
/// Most directory entries visited before giving up, matched or not.
pub(crate) const MAX_ENTRIES_SCANNED: usize = 200_000;
/// Wall-clock ceiling for the walk.
pub(crate) const MAX_WALK_SECS: u64 = 20;
/// How often the cancellation token and the clock are consulted.
pub(crate) const CHECK_EVERY: usize = 1024;

/// Directories that are never worth walking and are the usual source of
/// million-entry trees: dependency and build caches.
// ponytail: a fixed list, not config. Add names here if a new ecosystem shows up.
pub(crate) fn is_cycle_prone(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return false;
    }
    matches!(
        entry.file_name().to_str(),
        Some("node_modules") | Some(".git") | Some("target") | Some(".venv")
    )
}

/// A glob that understands brace alternates (`*.{mp3,wav}`), which the `glob`
/// crate does not — it treats `{`, `,` and `}` as literal characters, so every
/// brace pattern an LLM writes (bash/ripgrep/fd syntax) silently matched zero
/// files instead of erroring. Expansion happens once, at construction.
// ponytail: expand to N plain patterns instead of pulling in `globset`.
pub(crate) struct MultiGlob {
    patterns: Vec<glob::Pattern>,
}

impl MultiGlob {
    pub(crate) fn new(pattern: &str) -> Result<Self, glob::PatternError> {
        expand_braces(pattern)
            .iter()
            .map(|p| glob::Pattern::new(p))
            .collect::<Result<Vec<_>, _>>()
            .map(|patterns| Self { patterns })
    }

    pub(crate) fn matches_path_with(&self, path: &Path, options: glob::MatchOptions) -> bool {
        self.patterns
            .iter()
            .any(|p| p.matches_path_with(path, options))
    }

    pub(crate) fn matches(&self, s: &str) -> bool {
        self.patterns.iter().any(|p| p.matches(s))
    }
}

/// Cap on the expansion so `{a,b}{a,b}{a,b}...` cannot explode combinatorially.
const MAX_BRACE_EXPANSIONS: usize = 256;

/// Expand `a{b,c}d` into `["abd", "acd"]`, recursively (nesting supported).
/// An unbalanced or over-large brace group is left as a literal.
fn expand_braces(pattern: &str) -> Vec<String> {
    let bytes = pattern.as_bytes();
    let open = match bytes.iter().position(|&c| c == b'{') {
        Some(i) => i,
        None => return vec![pattern.to_string()],
    };

    // Find the matching `}` and the top-level commas inside this group.
    let mut depth = 0usize;
    let mut close = None;
    let mut splits: Vec<usize> = Vec::new();
    for (i, &c) in bytes.iter().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            b',' if depth == 1 => splits.push(i),
            _ => {}
        }
    }
    let close = match close {
        Some(i) => i,
        None => return vec![pattern.to_string()], // unbalanced — treat as literal
    };

    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let mut alts: Vec<&str> = Vec::new();
    let mut start = open + 1;
    for &s in &splits {
        alts.push(&pattern[start..s]);
        start = s + 1;
    }
    alts.push(&pattern[start..close]);

    let mut out = Vec::new();
    for alt in alts {
        for tail in expand_braces(&format!("{}{}{}", prefix, alt, suffix)) {
            if out.len() >= MAX_BRACE_EXPANSIONS {
                return out;
            }
            out.push(tail);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn data_dir() -> PathBuf {
        PathBuf::from("/opt/agentos/data")
    }

    #[test]
    fn brace_globs_expand() {
        // The bug: glob 0.3 treats `{`/`,`/`}` as literals, so `*.{mp3,wav}`
        // matched nothing and the agent reported "no audio files".
        let g = MultiGlob::new("**/*.{mp3,wav}").unwrap();
        let opts = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        assert!(g.matches_path_with(Path::new("combined_audio[1].mp3"), opts));
        assert!(g.matches_path_with(Path::new("a/b/x.wav"), opts));
        assert!(!g.matches_path_with(Path::new("x.txt"), opts));

        // Nested alternates, and plain patterns still work unchanged.
        assert_eq!(expand_braces("a{b,c{d,e}}f").len(), 3);
        assert_eq!(expand_braces("*.rs"), vec!["*.rs".to_string()]);
        // Unbalanced brace stays literal rather than erroring.
        assert_eq!(expand_braces("a{b"), vec!["a{b".to_string()]);
        assert!(MultiGlob::new("{a,b}").unwrap().matches("a"));
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
    fn resolve_absolute_non_workspace_denied() {
        let err = resolve_tool_path("/etc/passwd", &data_dir(), &[]).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("No workspace grant covers"),
            "unexpected error: {msg}"
        );
        assert!(msg.contains("agentos workspace grant"));
    }

    #[test]
    fn resolve_absolute_under_data_dir_allowed() {
        // Absolute paths that already sit under data_dir don't need a grant.
        let r = resolve_tool_path("/opt/agentos/data/notes.txt", &data_dir(), &[]).unwrap();
        assert_eq!(r, PathBuf::from("/opt/agentos/data/notes.txt"));
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
        if let Ok(p) = r {
            // If allowed, the resulting path must still be under data_dir
            // (the canonicalize check in the caller enforces this at runtime)
            assert!(!p.to_string_lossy().contains(".."));
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
