//! Runtime resolver — locate a working `node`/`python` binary that satisfies a
//! minimum version, walking known runtime managers (nvm, volta, asdf) before
//! falling back to the system `PATH`.
//!
//! Why this exists: when a user's shell has nvm/volta/asdf loaded, their
//! interactive `PATH` points at a modern runtime, but the AgentOS kernel may
//! have been started from a different shell (service manager, another terminal)
//! where that manager was never sourced — leaving an ancient system `node` as
//! whatever `#!/usr/bin/env node` resolves to. That mismatch produced the
//! 2026-04-18 Gmail MCP incident (`SyntaxError: Unexpected token '.'` surfaced
//! as `"MCP server closed connection unexpectedly"`).
//!
//! The fix: explicitly enumerate runtime-manager install roots, probe each
//! candidate's `--version`, pick the highest one meeting a declared minimum,
//! and hand the install command an **absolute** path — bypassing shebang
//! lookup entirely. The stdio transport itself stays runtime-agnostic; only the
//! install command (Phase 4) calls this resolver.

use agentos_types::AgentOSError;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a resolved runtime binary came from, in resolution-preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum RuntimeSource {
    /// `~/.agentos/runtimes/<name>-<version>/bin/<name>` (populated by Phase 7).
    Bundled,
    /// `~/.nvm/versions/node/*/bin/node`.
    Nvm,
    /// `~/.volta/tools/image/node/*/bin/node`.
    Volta,
    /// `~/.asdf/installs/nodejs/*/bin/node`.
    Asdf,
    /// Resolved via the process `PATH`.
    System,
}

/// A runtime binary that satisfies the requested minimum version.
#[derive(Debug, Clone)]
pub struct ResolvedRuntime {
    /// Absolute path to the runtime binary.
    pub binary: PathBuf,
    /// Parsed version string, e.g. `"20.20.1"`.
    pub version: String,
    /// Which manager/source the binary came from.
    pub source: RuntimeSource,
}

/// Resolve `node` (>= `min_version`), preferring the highest satisfying binary.
pub fn resolve_node(min_version: &str) -> Result<ResolvedRuntime, AgentOSError> {
    resolve("node", min_version, &node_candidates())
}

/// Resolve `python`/`python3` (>= `min_version`).
pub fn resolve_python(min_version: &str) -> Result<ResolvedRuntime, AgentOSError> {
    resolve("python", min_version, &python_candidates())
}

/// Resolve a runtime by name (`"node"`, `"python"`, `"python3"`). Used by the
/// install command to dispatch off a catalog entry's declared runtime.
pub fn resolve_by_name(runtime: &str, min: &str) -> Result<ResolvedRuntime, AgentOSError> {
    match runtime {
        "node" => resolve_node(min),
        "python" | "python3" => resolve_python(min),
        other => Err(AgentOSError::RuntimeNotFound {
            name: other.into(),
            min_version: min.into(),
        }),
    }
}

fn resolve(
    name: &str,
    min_version: &str,
    candidates: &[(RuntimeSource, PathBuf)],
) -> Result<ResolvedRuntime, AgentOSError> {
    let mut best: Option<ResolvedRuntime> = None;
    for (source, path) in candidates {
        if let Some(version) = probe_version(path) {
            if version_at_least(&version, min_version)
                && best
                    .as_ref()
                    .is_none_or(|b| version_gt(&version, &b.version))
            {
                best = Some(ResolvedRuntime {
                    binary: path.clone(),
                    version,
                    source: *source,
                });
            }
        }
    }

    match best {
        Some(r) => {
            tracing::info!(
                binary = %r.binary.display(),
                version = %r.version,
                source = ?r.source,
                "Resolved {name} runtime"
            );
            Ok(r)
        }
        None => Err(AgentOSError::RuntimeNotFound {
            name: name.into(),
            min_version: min_version.into(),
        }),
    }
}

/// Home directory from `$HOME` (these runtime-manager layouts are Unix-only).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Enumerate `<root>/*/bin/<name>` for every immediate child of `root`.
fn versioned_bins(
    root: &Path,
    name: &str,
    source: RuntimeSource,
    out: &mut Vec<(RuntimeSource, PathBuf)>,
) {
    if !root.is_dir() {
        return;
    }
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path().join("bin").join(name);
        if path.is_file() {
            out.push((source, path));
        }
    }
}

fn node_candidates() -> Vec<(RuntimeSource, PathBuf)> {
    let mut out = Vec::new();

    if let Some(home) = home_dir() {
        // Bundled (Phase 7 populates ~/.agentos/runtimes/<name>-<version>/bin/node).
        versioned_bins(
            &home.join(".agentos/runtimes"),
            "node",
            RuntimeSource::Bundled,
            &mut out,
        );
        versioned_bins(
            &home.join(".nvm/versions/node"),
            "node",
            RuntimeSource::Nvm,
            &mut out,
        );
        versioned_bins(
            &home.join(".volta/tools/image/node"),
            "node",
            RuntimeSource::Volta,
            &mut out,
        );
        versioned_bins(
            &home.join(".asdf/installs/nodejs"),
            "node",
            RuntimeSource::Asdf,
            &mut out,
        );
    }

    if let Some(path) = which_system("node") {
        out.push((RuntimeSource::System, path));
    }

    out
}

fn python_candidates() -> Vec<(RuntimeSource, PathBuf)> {
    let mut out = Vec::new();

    if let Some(home) = home_dir() {
        // pyenv: ~/.pyenv/versions/<ver>/bin/python3 (optional; system usually suffices).
        versioned_bins(
            &home.join(".pyenv/versions"),
            "python3",
            RuntimeSource::Asdf,
            &mut out,
        );
        versioned_bins(
            &home.join(".asdf/installs/python"),
            "python3",
            RuntimeSource::Asdf,
            &mut out,
        );
    }

    // Prefer python3, then fall back to python.
    if let Some(path) = which_system("python3") {
        out.push((RuntimeSource::System, path));
    } else if let Some(path) = which_system("python") {
        out.push((RuntimeSource::System, path));
    }

    out
}

/// Walk `$PATH` for an executable named `name`, returning the first hit.
fn which_system(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Run `<binary> --version` and parse the version number out of the output.
fn probe_version(binary: &Path) -> Option<String> {
    let out = Command::new(binary).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    // node prints to stdout ("v20.20.1"); some pythons print to stderr.
    let raw = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr)
    } else {
        String::from_utf8_lossy(&out.stdout)
    };
    let version = raw
        .trim()
        .trim_start_matches('v')
        .trim_start_matches("Python ")
        .trim()
        .to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Parse a dotted version into numeric components, ignoring non-numeric tails
/// (e.g. `"3.11.4rc1"` → `[3, 11]`).
fn version_parts(s: &str) -> Vec<u32> {
    s.split('.').map_while(|p| p.parse().ok()).collect()
}

fn version_at_least(got: &str, min: &str) -> bool {
    version_parts(got) >= version_parts(min)
}

fn version_gt(a: &str, b: &str) -> bool {
    version_parts(a) > version_parts(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn version_at_least_handles_majors_and_minors() {
        assert!(version_at_least("20.20.1", "18"));
        assert!(!version_at_least("12.22.9", "18"));
        assert!(version_at_least("3.11.4", "3.10"));
        assert!(!version_at_least("3.9.1", "3.10"));
        assert!(version_at_least("18", "18"));
    }

    #[test]
    fn version_gt_orders_correctly() {
        assert!(version_gt("20.0.0", "18.20.5"));
        assert!(!version_gt("18.20.5", "20.0.0"));
        assert!(!version_gt("18.0.0", "18.0.0"));
    }

    /// Write a fake `node`-like script that prints `version` to stdout.
    fn fake_runtime(dir: &Path, name: &str, version: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh\necho \"v{version}\"").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn resolve_picks_highest_satisfying_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let old = fake_runtime(tmp.path(), "node-old", "18.20.5");
        let new = fake_runtime(tmp.path(), "node-new", "20.20.1");
        let candidates = vec![
            (RuntimeSource::System, old),
            (RuntimeSource::Nvm, new.clone()),
        ];
        let resolved = resolve("node", "18", &candidates).unwrap();
        assert_eq!(resolved.binary, new);
        assert_eq!(resolved.version, "20.20.1");
        assert_eq!(resolved.source, RuntimeSource::Nvm);
    }

    #[test]
    fn resolve_rejects_when_all_below_minimum() {
        let tmp = tempfile::tempdir().unwrap();
        let old = fake_runtime(tmp.path(), "node", "12.22.9");
        let candidates = vec![(RuntimeSource::System, old)];
        let err = resolve("node", "18", &candidates).unwrap_err();
        assert!(matches!(err, AgentOSError::RuntimeNotFound { .. }));
    }

    #[test]
    fn resolve_rejects_when_no_candidates() {
        let err = resolve("node", "18", &[]).unwrap_err();
        assert!(matches!(
            err,
            AgentOSError::RuntimeNotFound { ref name, .. } if name == "node"
        ));
    }

    #[test]
    fn probe_version_parses_node_and_python_styles() {
        let tmp = tempfile::tempdir().unwrap();
        let node = fake_runtime(tmp.path(), "node", "20.20.1");
        assert_eq!(probe_version(&node).as_deref(), Some("20.20.1"));

        // Python-style "Python 3.11.4" output.
        let py = tmp.path().join("python3");
        {
            let mut f = std::fs::File::create(&py).unwrap();
            writeln!(f, "#!/bin/sh\necho \"Python 3.11.4\"").unwrap();
        } // close the handle before exec to avoid ETXTBSY
        let mut perms = std::fs::metadata(&py).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&py, perms).unwrap();
        assert_eq!(probe_version(&py).as_deref(), Some("3.11.4"));
    }

    #[test]
    fn resolve_by_name_rejects_unknown_runtime() {
        let err = resolve_by_name("ruby", "3").unwrap_err();
        assert!(matches!(
            err,
            AgentOSError::RuntimeNotFound { ref name, .. } if name == "ruby"
        ));
    }
}
