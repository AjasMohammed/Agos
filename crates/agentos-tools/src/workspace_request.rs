use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Ask the operator for access to a host folder.
///
/// The gap this fills: grants are written only by the CLI and the REST surface
/// (`cmd_grant_workspace`), so an agent that needs a folder has no verb for it.
/// On 2026-09-21 two agents spent four turns describing a grant to each other
/// that neither could issue, and the operator approved three `shell-exec`
/// escalations that changed no access at all. A model with no way to say what it
/// needs will narrate it instead.
///
/// The call parks until the operator decides; approval writes a real
/// `WorkspaceGrant` before the caller wakes. Requires `fs.workspace:r` — an
/// agent that may never touch host folders should not be able to ask.
pub struct WorkspaceRequestTool;

impl WorkspaceRequestTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WorkspaceRequestTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for WorkspaceRequestTool {
    fn name(&self) -> &str {
        "workspace-request"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("user.interact".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let path = payload
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("workspace-request requires 'path' field".into())
            })?
            .trim()
            .to_string();
        let reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "workspace-request requires 'reason' — the operator decides on it".into(),
                )
            })?
            .to_string();
        let mode = payload
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("r")
            .to_ascii_lowercase();

        validate_request(&path, &mode, &context.data_dir)?;

        let timeout_secs = payload
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(300)
            .clamp(30, 3600);

        Ok(serde_json::json!({
            "_kernel_action": "workspace_request",
            "path": path,
            "mode": mode,
            "reason": reason,
            "timeout_secs": timeout_secs,
        }))
    }
}

/// Reject what the operator must never be asked to approve, before the question
/// is ever raised. Re-checked kernel-side at resolve time: this payload came
/// from a model, and the metadata that reaches `resolve` must not be trusted
/// because a tool validated it once.
pub fn validate_request(
    path: &str,
    mode: &str,
    data_dir: &std::path::Path,
) -> Result<(), AgentOSError> {
    let deny = |operation: String| AgentOSError::PermissionDenied {
        resource: "fs.workspace".into(),
        operation,
    };
    if !matches!(mode, "r" | "rw" | "rwx") {
        return Err(AgentOSError::SchemaValidation(format!(
            "invalid mode '{mode}': expected 'r', 'rw' or 'rwx'"
        )));
    }
    let p = std::path::Path::new(path);
    if !p.is_absolute() {
        return Err(deny(format!(
            "workspace-request needs an absolute path, got '{path}'"
        )));
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(deny(format!("path traversal denied: '{path}'")));
    }
    // The kernel data directory is off limits in BOTH directions, and the two
    // are different failures:
    //
    // - An ancestor (`/`, `/home`, the data dir's parent) drags the vault, the
    //   audit log and every agent home in behind it. `grants_outside` drops
    //   such a grant before a sandbox binds it, but a file tool would still
    //   honour it, so it is refused here.
    // - A DESCENDANT is the one that matters most and the one nothing else
    //   catches: `<data_dir>/agents` reads as an ordinary folder in the
    //   approval card, and granting it hands this agent every other agent's
    //   home — the exact containment this whole feature promises. No deny list
    //   downstream knows where `data_dir` is, so the refusal has to be here.
    //
    // Compared lexically first (the path need not exist yet), then again on
    // canonical forms so a symlink cannot walk in sideways.
    let canonical_data = std::fs::canonicalize(data_dir).ok();
    let canonical_p = std::fs::canonicalize(p).ok();
    let overlaps_data_dir = data_dir.starts_with(p)
        || p.starts_with(data_dir)
        || canonical_data
            .as_ref()
            .zip(canonical_p.as_ref())
            .is_some_and(|(d, g)| d.starts_with(g) || g.starts_with(d));
    if overlaps_data_dir {
        return Err(deny(format!(
            "'{path}' is inside or above the kernel data directory. That space holds the vault, \
             the audit log and every agent's private home; it is never grantable. Ask for the \
             folder you actually need, or use this conversation's shared workspace."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from("/var/lib/agentos/data")
    }

    #[test]
    fn relative_paths_refused() {
        assert!(validate_request("Desktop", "r", &data_dir()).is_err());
    }

    #[test]
    fn traversal_refused() {
        assert!(validate_request("/tmp/../etc", "r", &data_dir()).is_err());
    }

    #[test]
    fn ancestor_of_data_dir_refused() {
        assert!(validate_request("/var/lib/agentos", "rw", &data_dir()).is_err());
        assert!(validate_request("/", "r", &data_dir()).is_err());
    }

    #[test]
    fn bad_mode_refused() {
        assert!(validate_request("/tmp/x", "wx", &data_dir()).is_err());
    }

    /// The direction nothing downstream catches: `<data_dir>/agents` looks like
    /// an ordinary folder on the approval card and would hand over every other
    /// agent's home.
    #[test]
    fn descendant_of_data_dir_refused() {
        for p in [
            "/var/lib/agentos/data/agents",
            "/var/lib/agentos/data/agents/beta",
            "/var/lib/agentos/data/vault",
            "/var/lib/agentos/data",
        ] {
            assert!(
                validate_request(p, "rw", &data_dir()).is_err(),
                "{p} must not be grantable"
            );
        }
    }

    #[test]
    fn ordinary_request_accepted() {
        assert!(validate_request("/home/op/Desktop", "rw", &data_dir()).is_ok());
    }
}
