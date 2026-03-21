use agentos_types::AgentOSError;
use std::path::{Component, Path, PathBuf};

/// Indicates which access zone a resolved path falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathZone {
    /// Path is inside the agent's sandboxed data directory.
    DataDir,
    /// Path is inside a configured workspace directory.
    Workspace,
}

/// Permission resource name required for workspace directory access.
pub const WORKSPACE_PERMISSION: &str = "fs.workspace";

/// Resolve and validate a path for a **read** operation (path must exist).
///
/// Resolution rules:
/// 1. If `path_str` is absolute and canonicalizes into a configured workspace
///    root, the path is accepted and `PathZone::Workspace` is returned.
/// 2. Otherwise the path is joined with `data_dir` using the existing convention
///    (strip leading `/` for absolute paths; join relative paths directly).
///    The canonical result must fall within `data_dir`.
///
/// Path traversal (`..`) is implicitly rejected by the canonicalization
/// containment check.  A `..` that escapes any allowed root will cause the
/// function to return `PermissionDenied`.
pub fn resolve_path_existing(
    path_str: &str,
    tool_name: &str,
    data_dir: &Path,
    workspace_paths: &[PathBuf],
) -> Result<(PathBuf, PathZone), AgentOSError> {
    let requested = Path::new(path_str);

    // Workspace candidate: only absolute paths are checked against workspace
    // roots.  Relative paths are always resolved relative to data_dir.
    if requested.is_absolute() && !workspace_paths.is_empty() {
        if let Ok(canonical) = requested.canonicalize() {
            for wp in workspace_paths {
                if let Ok(canonical_wp) = wp.canonicalize() {
                    if canonical.starts_with(&canonical_wp) {
                        return Ok((canonical, PathZone::Workspace));
                    }
                }
            }
            // Absolute path exists but is not under any workspace root; fall
            // through to data_dir resolution which will also produce a
            // containment error — this gives a clear "traversal denied" message.
        }
        // canonicalize failed → path does not exist yet.  Fall through to the
        // data_dir resolution path which will canonicalize after joining and
        // return "Path not found".
    }

    // Standard data_dir resolution: strip leading `/` then join.
    let resolved = if requested.is_absolute() {
        let stripped = requested.strip_prefix("/").unwrap_or(requested);
        data_dir.join(stripped)
    } else {
        data_dir.join(requested)
    };

    let canonical = resolved
        .canonicalize()
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: tool_name.to_string(),
            reason: format!("Path not found: {} ({})", path_str, e),
        })?;

    let canonical_data_dir =
        data_dir
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: tool_name.to_string(),
                reason: format!("Data directory error: {}", e),
            })?;

    if !canonical.starts_with(&canonical_data_dir) {
        return Err(AgentOSError::PermissionDenied {
            resource: "fs.user_data".into(),
            operation: format!("Path traversal denied: {}", path_str),
        });
    }

    Ok((canonical, PathZone::DataDir))
}

/// Resolve and validate a path for a **write** operation (path may not exist).
///
/// Uses lexical normalization instead of `canonicalize()` because the target
/// path may not exist yet.
///
/// Resolution rules:
/// 1. If `path_str` is absolute and its normalized form falls within a
///    configured workspace directory (the root must exist to canonicalize),
///    returns `PathZone::Workspace`.
/// 2. Otherwise uses standard data_dir resolution.
///
/// Traversal attempts such as `/workspace/../etc/passwd` normalize to
/// `/etc/passwd` which will not start-with any workspace root and are
/// immediately rejected with `PermissionDenied` (no silent data_dir rebase).
pub fn resolve_path_writable(
    path_str: &str,
    tool_name: &str,
    data_dir: &Path,
    workspace_paths: &[PathBuf],
) -> Result<(PathBuf, PathZone), AgentOSError> {
    let requested = Path::new(path_str);

    if requested.is_absolute() && !workspace_paths.is_empty() {
        let normalized = normalize_path(requested);
        for wp in workspace_paths {
            if let Ok(canonical_wp) = wp.canonicalize() {
                if normalized.starts_with(&canonical_wp) {
                    return Ok((normalized, PathZone::Workspace));
                }
            }
        }
        // Absolute path does not fall within any configured workspace.
        // Reject outright — silently rebasing under data_dir would allow
        // traversal attacks to slip through (e.g. /workspace/../../etc/passwd
        // normalizes to /etc/passwd, then gets rebased to data_dir/etc/passwd
        // and passes the containment check).
        return Err(AgentOSError::PermissionDenied {
            resource: "fs.user_data".into(),
            operation: format!("Path traversal denied: {}", path_str),
        });
    }

    // Standard data_dir resolution (lexical normalization — file may not exist).
    let resolved = if requested.is_absolute() {
        let stripped = requested.strip_prefix("/").unwrap_or(requested);
        data_dir.join(stripped)
    } else {
        data_dir.join(requested)
    };

    let normalized = normalize_path(&resolved);

    let canonical_data_dir =
        data_dir
            .canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: tool_name.to_string(),
                reason: format!("Data directory error: {}", e),
            })?;

    if !normalized.starts_with(&canonical_data_dir) {
        return Err(AgentOSError::PermissionDenied {
            resource: "fs.user_data".into(),
            operation: format!("Path traversal denied: {}", path_str),
        });
    }

    Ok((normalized, PathZone::DataDir))
}

/// Lexically normalize a path by resolving `.` and `..` without touching the
/// filesystem.  Used for write targets that may not exist yet.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other),
        }
    }
    result
}

/// Validate workspace path configuration entries at kernel startup.
///
/// Enforces:
/// - Each path must be absolute.
/// - No system-critical root directories (`/`, `/etc`, `/var`, `/root`, etc.).
pub fn validate_workspace_paths(paths: &[PathBuf]) -> Result<(), String> {
    const FORBIDDEN: &[&str] = &[
        "/", "/etc", "/var", "/root", "/home", "/sys", "/proc", "/dev", "/boot", "/usr",
    ];
    for path in paths {
        if !path.is_absolute() {
            return Err(format!(
                "workspace.allowed_paths entry '{}' must be an absolute path",
                path.display()
            ));
        }
        let path_str = path.to_string_lossy();
        for forbidden in FORBIDDEN {
            if path_str == *forbidden {
                return Err(format!(
                    "workspace.allowed_paths entry '{}' is a protected system directory \
                     and cannot be used as a workspace root",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_resolve_existing_relative_in_data_dir() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let file = data.join("hello.txt");
        std::fs::write(&file, "hi").unwrap();

        let (resolved, zone) = resolve_path_existing("hello.txt", "test", &data, &[]).unwrap();
        assert_eq!(zone, PathZone::DataDir);
        assert_eq!(resolved, file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_existing_absolute_in_workspace() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let src = workspace.join("main.rs");
        std::fs::write(&src, "fn main() {}").unwrap();

        let (resolved, zone) = resolve_path_existing(
            src.to_str().unwrap(),
            "test",
            &data,
            std::slice::from_ref(&workspace),
        )
        .unwrap();
        assert_eq!(zone, PathZone::Workspace);
        assert_eq!(resolved, src.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_existing_no_workspace_rejects_outside_data() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        // A file that exists but is outside data_dir AND no workspace configured.
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();

        // Without workspace, the absolute path gets rebase'd under data_dir
        // and therefore won't be found (double-nested path).
        let result = resolve_path_existing(outside.to_str().unwrap(), "test", &data, &[]);
        assert!(
            result.is_err(),
            "absolute path outside data_dir must be rejected"
        );
    }

    #[test]
    fn test_resolve_writable_new_workspace_file() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let new_file = workspace.join("new.rs");

        let (resolved, zone) = resolve_path_writable(
            new_file.to_str().unwrap(),
            "test",
            &data,
            std::slice::from_ref(&workspace),
        )
        .unwrap();
        assert_eq!(zone, PathZone::Workspace);
        assert_eq!(resolved, new_file); // lexically normalized
    }

    #[test]
    fn test_resolve_writable_traversal_blocked() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();

        // /project/../../../etc/passwd normalizes to /etc/passwd — not in workspace.
        let evil_path = format!("{}/../../etc/passwd", workspace.display());
        let result = resolve_path_writable(&evil_path, "test", &data, &[workspace]);
        assert!(result.is_err(), "traversal via workspace must be rejected");
    }

    #[test]
    fn test_validate_workspace_paths_ok() {
        validate_workspace_paths(&[PathBuf::from("/home/user/project")]).unwrap();
    }

    #[test]
    fn test_validate_workspace_paths_rejects_relative() {
        let result = validate_workspace_paths(&[PathBuf::from("relative/path")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_workspace_paths_rejects_etc() {
        let result = validate_workspace_paths(&[PathBuf::from("/etc")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_workspace_paths_rejects_root() {
        let result = validate_workspace_paths(&[PathBuf::from("/")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_normalize_path_resolves_dotdot() {
        let p = PathBuf::from("/foo/bar/../baz");
        assert_eq!(normalize_path(&p), PathBuf::from("/foo/baz"));
    }
}
