use agentos_types::{AgentOSError, PermissionOp};
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

/// Reserved payload key: where a HAL driver puts captures when the agent
/// gave no `output_path`. Stamped by the tool wrapper from the agent home so
/// frames and recordings never default to a world-readable `/tmp`.
pub const HAL_OUTPUT_DIR_KEY: &str = "__output_dir";

/// The agent's `captures/` directory, created and canonical.
pub fn agent_capture_dir(
    tool_name: &str,
    context: &crate::traits::ToolExecutionContext,
) -> Result<PathBuf, AgentOSError> {
    let dir = context.agent_files_dir()?.join("captures");
    std::fs::create_dir_all(&dir)
        .and_then(|_| dir.canonicalize())
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: tool_name.to_string(),
            reason: format!("Cannot create {}: {e}", dir.display()),
        })
}

/// Contain a HAL payload path (`output_path`, `audio_path`) before it reaches
/// a driver. Drivers read and write wherever they are told, so this is their
/// only containment: a relative path lands in the agent home; an absolute one
/// must already lie in the home or a granted workspace folder (which also
/// needs the `fs.workspace` permission, as for the file tools). A write
/// target gets its parent directory created and re-checked after
/// canonicalization, so a symlink inside the home cannot point the write out.
pub fn contain_hal_path(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    tool_name: &str,
    context: &crate::traits::ToolExecutionContext,
    writable: bool,
) -> Result<(), AgentOSError> {
    let Some(raw) = map.get(key).and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    if raw.is_empty() {
        // The driver reports the empty string itself.
        return Ok(());
    }
    let home = context.agent_files_dir()?;
    let home = home
        .canonicalize()
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: tool_name.to_string(),
            reason: format!("Agent home error: {} ({e})", home.display()),
        })?;
    let (granted, op) = if writable {
        (&context.workspace_paths_writable, PermissionOp::Write)
    } else {
        (&context.workspace_paths, PermissionOp::Read)
    };
    let roots: Vec<PathBuf> = std::iter::once(home.clone())
        .chain(granted.iter().cloned())
        .collect();
    let (mut resolved, zone) = if writable {
        resolve_path_writable(raw, tool_name, &home, &roots)?
    } else {
        resolve_path_existing(raw, tool_name, &home, &roots)?
    };
    if zone == PathZone::Workspace
        && !resolved.starts_with(&home)
        && !context.permissions.check(WORKSPACE_PERMISSION, op)
    {
        return Err(AgentOSError::PermissionDenied {
            resource: WORKSPACE_PERMISSION.into(),
            operation: format!("Workspace access denied: {raw}"),
        });
    }
    if writable {
        let Some(parent) = resolved.parent().map(Path::to_path_buf) else {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied: {raw}"),
            });
        };
        std::fs::create_dir_all(&parent)
            .and_then(|_| parent.canonicalize())
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: tool_name.to_string(),
                reason: format!("Cannot create {}: {e}", parent.display()),
            })
            .and_then(|canonical_parent| {
                let inside = roots.iter().any(|root| {
                    root.canonicalize()
                        .map(|r| canonical_parent.starts_with(r))
                        .unwrap_or(false)
                });
                let name = resolved.file_name().map(|n| n.to_os_string());
                match (inside, name) {
                    (true, Some(name)) => {
                        resolved = canonical_parent.join(name);
                        Ok(())
                    }
                    _ => Err(AgentOSError::PermissionDenied {
                        resource: "fs.user_data".into(),
                        operation: format!("Path traversal denied: {raw}"),
                    }),
                }
            })?;
    }
    map.insert(
        key.to_string(),
        serde_json::Value::String(resolved.to_string_lossy().into_owned()),
    );
    Ok(())
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

/// Agent-home subdirectory holding recoverable copies of deleted and
/// overwritten files. Entries are named `<unix_secs>-<uuid8>-<file_name>`.
pub(crate) const TRASH_DIR: &str = ".trash";
const TRASH_TTL_SECS: u64 = 72 * 3600;
/// Above this, a cross-filesystem delete/backup is not copied into the trash.
const TRASH_COPY_MAX_BYTES: u64 = 10 * 1024 * 1024;
const TRASH_MAX_ENTRIES: usize = 200;
const TRASH_MAX_BYTES: u64 = 500 * 1024 * 1024;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Reserve a fresh path in the agent's trash and prune it: entries past the
/// TTL go, then oldest-first until it is under the entry and byte caps.
// ponytail: prune-on-use instead of a kernel sweeper, and a directory entry
// counts as 0 bytes. Add a TimeoutChecker sweep / du walk if that ever matters.
async fn trash_slot(agent_root: &Path, original: &Path) -> std::io::Result<PathBuf> {
    let dir = agent_root.join(TRASH_DIR);
    tokio::fs::create_dir_all(&dir).await?;
    // SECURITY: the agent can plant `.trash` as a symlink (shell-exec has its
    // home bound rw). Everything below deletes and renames by path with kernel
    // privileges, so the resolved directory must still be inside the home.
    let dir = tokio::fs::canonicalize(&dir).await?;
    if !dir.starts_with(tokio::fs::canonicalize(agent_root).await?) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            ".trash resolves outside the agent home",
        ));
    }

    let now = now_secs();
    // (stamp, path, is_dir, bytes) for every entry this module created.
    let mut kept: Vec<(u64, PathBuf, bool, u64)> = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(stamp) = parse_trash_stamp(&name.to_string_lossy()) else {
            continue;
        };
        // DirEntry metadata does not follow symlinks.
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let bytes = if meta.is_file() { meta.len() } else { 0 };
        kept.push((stamp, entry.path(), meta.is_dir(), bytes));
    }
    kept.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut total = 0u64;
    for (i, (stamp, path, is_dir, bytes)) in kept.into_iter().enumerate() {
        total += bytes;
        let expired = now.saturating_sub(stamp) > TRASH_TTL_SECS;
        if expired || i >= TRASH_MAX_ENTRIES || total > TRASH_MAX_BYTES {
            let _ = if is_dir {
                tokio::fs::remove_dir_all(&path).await
            } else {
                tokio::fs::remove_file(&path).await
            };
        }
    }

    let name = original
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut cut = name.len().min(96);
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    Ok(dir.join(format!("{}-{}-{}", now, &id[..8], &name[..cut])))
}

/// `<unix_secs>-<8 hex>-<name>` → the stamp. Anything else is not ours to prune.
fn parse_trash_stamp(name: &str) -> Option<u64> {
    let mut parts = name.splitn(3, '-');
    let stamp = parts.next()?.parse().ok()?;
    let id = parts.next()?;
    parts.next()?;
    (id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(stamp)
}

fn trash_display(slot: &Path) -> String {
    let name = slot.file_name().unwrap_or_default().to_string_lossy();
    format!("{}/{}", TRASH_DIR, name)
}

/// Copy a regular file to a path that must not exist yet.
///
/// SECURITY: the destination is opened with `create_new` (O_EXCL), which fails
/// on any existing entry including a dangling symlink. `fs::copy` would follow
/// such a link and write outside the granted roots.
pub(crate) async fn copy_excl(from: &Path, to: &Path) -> Result<(), String> {
    let meta = tokio::fs::metadata(from)
        .await
        .map_err(|e| format!("Cannot stat source: {}", e))?;
    if !meta.is_file() {
        return Err("Only regular files can be copied; directories are not supported".into());
    }
    let mut src = tokio::fs::File::open(from)
        .await
        .map_err(|e| format!("Cannot open source: {}", e))?;
    let mut dst = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .await
        .map_err(|e| format!("Cannot create destination: {}", e))?;
    if let Err(e) = tokio::io::copy(&mut src, &mut dst).await {
        drop(dst);
        let _ = tokio::fs::remove_file(to).await;
        return Err(format!("Copy failed: {}", e));
    }
    drop(dst);
    // Best-effort: vfat/exfat reject chmod.
    let _ = tokio::fs::set_permissions(to, plain_mode(&meta)).await;
    Ok(())
}

/// Cross-filesystem move of a regular file: [`copy_excl`] then remove the source.
pub(crate) async fn copy_then_remove(from: &Path, to: &Path) -> Result<(), String> {
    copy_excl(from, to).await?;
    tokio::fs::remove_file(from).await.map_err(|e| {
        format!(
            "Copied to the destination but could not remove the source: {}. \
             The file now exists at both paths.",
            e
        )
    })
}

/// Delete `path` (file or directory) by moving it into the agent's trash and
/// return the home-relative trash path. Fails, leaving `path` untouched, when
/// the entry cannot be kept: a directory, or a file over 10 MiB, on a different
/// filesystem from the agent home. The caller offers `permanent` for that.
pub(crate) async fn trash(
    tool_name: &str,
    agent_root: &Path,
    path: &Path,
) -> Result<String, AgentOSError> {
    let fail = |reason: String| AgentOSError::ToolExecutionFailed {
        tool_name: tool_name.into(),
        reason,
    };
    let slot = trash_slot(agent_root, path)
        .await
        .map_err(|e| fail(format!("Cannot prepare trash: {}", e)))?;
    match tokio::fs::rename(path, &slot).await {
        Ok(()) => return Ok(trash_display(&slot)),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(e) => return Err(fail(format!("Cannot delete {}: {}", path.display(), e))),
    }
    let meta = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|e| fail(format!("Cannot stat {}: {}", path.display(), e)))?;
    if meta.is_file() && meta.len() <= TRASH_COPY_MAX_BYTES {
        copy_then_remove(path, &slot).await.map_err(fail)?;
        return Ok(trash_display(&slot));
    }
    Err(fail(format!(
        "{} is on a different filesystem from your home and is a directory or over 10 MiB, \
         so it cannot be kept in .trash. Nothing was deleted. Pass permanent=true to delete \
         it with no undo.",
        path.display()
    )))
}

/// Keep the current content of `target` in the trash before it is overwritten.
/// Best-effort: a failed backup never blocks the write. Returns the
/// home-relative trash path when a copy was kept.
pub(crate) async fn backup(agent_root: &Path, target: &Path) -> Option<String> {
    // SECURITY: lstat — a symlink leaf must not be copied, or its (possibly
    // out-of-sandbox) target would become readable from the trash.
    let meta = tokio::fs::symlink_metadata(target).await.ok()?;
    if !meta.is_file() {
        return None;
    }
    let slot = trash_slot(agent_root, target).await.ok()?;
    // A hard link is free and keeps the old inode alive once the rename in
    // `atomic_write` replaces the directory entry.
    // A setuid/setgid inode is never linked in; `copy_excl` strips those bits.
    #[cfg(unix)]
    let linkable = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7000 == 0
    };
    #[cfg(not(unix))]
    let linkable = true;
    if linkable && tokio::fs::hard_link(target, &slot).await.is_ok() {
        return Some(trash_display(&slot));
    }
    if meta.len() <= TRASH_COPY_MAX_BYTES && copy_excl(target, &slot).await.is_ok() {
        return Some(trash_display(&slot));
    }
    None
}

/// Permissions of `meta` without setuid/setgid/sticky — a file an agent wrote
/// must never inherit those from the file it replaced.
pub(crate) fn plain_mode(meta: &std::fs::Metadata) -> std::fs::Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(meta.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    meta.permissions()
}

/// Write `content` to a uniquely named dot-prefixed sibling of `target`, then rename
/// it into place so readers never observe a partial write.
///
/// The temp name is per-call (`.<file_name>.<uuid>.tmp`): a name derived from
/// the target's stem would be shared by `a.rs` and `a.toml`, which hold
/// different write locks. An existing target's permissions are carried over so
/// editing a script does not clear its exec bit.
pub(crate) async fn atomic_write(
    tool_name: &str,
    target: &Path,
    content: &str,
) -> Result<(), AgentOSError> {
    let fail = |reason: String| AgentOSError::ToolExecutionFailed {
        tool_name: tool_name.into(),
        reason,
    };
    let file_name = target
        .file_name()
        .ok_or_else(|| AgentOSError::SchemaValidation("Path has no filename".into()))?;
    // Uniqueness comes from the uuid; bound the name part so a 255-byte file
    // name does not push the temp name past NAME_MAX.
    let name = file_name.to_string_lossy();
    let mut cut = name.len().min(96);
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    let tmp = target.with_file_name(format!(".{}.{}.tmp", &name[..cut], uuid::Uuid::new_v4()));

    let staged = async {
        tokio::fs::write(&tmp, content)
            .await
            .map_err(|e| fail(format!("Temp write failed: {}", e)))?;
        // Best-effort: vfat/exfat/some FUSE mounts reject chmod, and the write
        // matters more than the mode.
        if let Ok(meta) = tokio::fs::metadata(target).await {
            let _ = tokio::fs::set_permissions(&tmp, plain_mode(&meta)).await;
        }
        tokio::fs::rename(&tmp, target)
            .await
            .map_err(|e| fail(format!("Atomic rename failed: {}", e)))
    }
    .await;
    if staged.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    staged
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

    #[cfg(unix)]
    #[tokio::test]
    async fn atomic_write_keeps_mode_and_sibling_tmp_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("run.sh");
        let user_tmp = tmp.path().join("run.tmp");
        std::fs::write(&script, "old").unwrap();
        std::fs::write(&user_tmp, "mine").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        atomic_write("test", &script, "new").await.unwrap();

        assert_eq!(std::fs::read_to_string(&script).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&user_tmp).unwrap(), "mine");
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        // No staging file left behind.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2);
    }

    #[tokio::test]
    async fn trash_and_backup_keep_recoverable_copies_and_prune_old_ones() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let stale = home.join(TRASH_DIR).join("1-deadbeef-old.txt");
        let foreign = home.join(TRASH_DIR).join("2024-budget.xlsx");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, "ancient").unwrap();
        std::fs::write(&foreign, "not ours").unwrap();

        let f = home.join("notes.txt");
        std::fs::write(&f, "v1").unwrap();
        let kept = backup(home, &f).await.expect("backup kept");
        atomic_write("test", &f, "v2").await.unwrap();
        assert_eq!(std::fs::read_to_string(home.join(&kept)).unwrap(), "v1");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "v2");
        assert!(!stale.exists(), "entries past the TTL are pruned");
        assert!(
            foreign.exists(),
            "only <secs>-<8hex>-<name> entries are pruned"
        );

        let dir = home.join("proj");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), "a").unwrap();
        let trashed = trash("test", home, &dir).await.unwrap();
        assert!(!dir.exists());
        assert_eq!(
            std::fs::read_to_string(home.join(&trashed).join("sub/a.txt")).unwrap(),
            "a"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn trash_refuses_symlinked_trash_dir() {
        let tmp = TempDir::new().unwrap();
        let outside = tmp.path().join("documents");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let victim = outside.join("1-deadbeef-report.txt");
        std::fs::write(&victim, "operator file").unwrap();
        std::os::unix::fs::symlink(&outside, home.join(TRASH_DIR)).unwrap();
        let f = home.join("f.txt");
        std::fs::write(&f, "x").unwrap();

        assert!(trash("test", &home, &f).await.is_err());
        assert!(backup(&home, &f).await.is_none());
        assert!(f.exists());
        assert!(victim.exists(), "pruned outside the agent home");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backup_skips_symlink_leaf() {
        let tmp = TempDir::new().unwrap();
        let secret = tmp.path().join("secret");
        std::fs::write(&secret, "s3cret").unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let link = home.join("link");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(backup(&home, &link).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn atomic_write_strips_setuid_and_handles_max_length_name() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("x".repeat(255));
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o4755)).unwrap();

        atomic_write("test", &target, "new").await.unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o755);
    }

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
