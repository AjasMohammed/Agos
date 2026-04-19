/// Kernel handlers for runtime workspace path management.
///
/// `cmd_workspace_add`    — add a path to the live allowlist (no restart required).
/// `cmd_workspace_remove` — remove a path from the live allowlist.
/// `cmd_workspace_list`   — list all currently allowed workspace paths.
use std::path::PathBuf;

use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_tools::workspace::validate_workspace_paths;
use agentos_types::TraceID;

use crate::kernel::Kernel;

impl Kernel {
    /// Add a new path to the workspace allowlist at runtime.
    ///
    /// The path must be absolute and must not be a system root (`/`, `/etc`, etc.).
    /// It is canonicalized before being stored and validated; if canonicalization
    /// fails (path does not exist yet) the raw absolute path is stored so operators
    /// can pre-register directories before they are created.
    ///
    /// This change is **runtime-only** — it is not persisted to `config/default.toml`.
    /// To make it permanent, add the path to `tools.workspace.allowed_paths` in your
    /// config file.
    pub async fn cmd_workspace_add(&self, path: String) -> KernelResponse {
        let raw = PathBuf::from(&path);

        // Canonicalize if possible; fall back to the raw path.
        let canonical = raw.canonicalize().unwrap_or_else(|_| raw.clone());

        // Validate using the same rules as the boot-time check (absolute + not a
        // forbidden system root). We validate the canonical form so symlinks that
        // resolve to a forbidden directory are also rejected.
        if let Err(msg) = validate_workspace_paths(std::slice::from_ref(&canonical)) {
            return KernelResponse::Error { message: msg };
        }

        let mut paths = self
            .workspace_paths
            .write()
            .unwrap_or_else(|e| e.into_inner());

        // Deduplicate — check both canonical and raw forms in case a previous add
        // stored the raw path (e.g. the directory didn't exist at add time).
        if paths.iter().any(|p| p == &canonical || p == &raw) {
            return KernelResponse::Error {
                message: format!(
                    "Workspace path already in allowlist: {}",
                    canonical.display()
                ),
            };
        }

        paths.push(canonical.clone());
        drop(paths); // release lock before async audit write

        tracing::info!(path = %canonical.display(), "Workspace path added at runtime");

        self.audit_log(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::KernelConfigChanged,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "setting": "workspace_paths",
                "action": "add",
                "path": canonical.to_string_lossy(),
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });

        KernelResponse::Success {
            data: Some(serde_json::json!({ "path": canonical.to_string_lossy() })),
        }
    }

    /// Remove a path from the workspace allowlist at runtime.
    ///
    /// Matches against both the raw and canonicalized forms of the stored paths.
    ///
    /// This change is **runtime-only** — update `config/default.toml` to persist it.
    pub async fn cmd_workspace_remove(&self, path: String) -> KernelResponse {
        let target = PathBuf::from(&path);
        let canonical = target.canonicalize().unwrap_or_else(|_| target.clone());

        let mut paths = self
            .workspace_paths
            .write()
            .unwrap_or_else(|e| e.into_inner());

        let before = paths.len();
        // Try exact match first; fall back to canonical equivalence.
        paths.retain(|p| p != &target && p != &canonical);

        if paths.len() == before {
            return KernelResponse::Error {
                message: format!("Path not found in workspace allowlist: {path}"),
            };
        }
        drop(paths);

        tracing::info!(path = %path, "Workspace path removed at runtime");

        self.audit_log(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::KernelConfigChanged,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "setting": "workspace_paths",
                "action": "remove",
                "path": path,
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });

        KernelResponse::Success { data: None }
    }

    /// Return all currently allowed workspace paths.
    pub async fn cmd_workspace_list(&self) -> KernelResponse {
        let paths = self
            .workspace_paths
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let list: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        KernelResponse::WorkspacePaths(list)
    }
}
