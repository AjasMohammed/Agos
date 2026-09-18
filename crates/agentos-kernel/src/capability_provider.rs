//! Kernel-Mediated Capabilities (KMC) — Provider trait and core types.
//!
//! A `CapabilityProvider` is a kernel-level abstraction that mediates system
//! interactions on behalf of agents. Instead of granting agents raw OS access,
//! the kernel exposes typed, audited, policy-controlled capability domains
//! (environments, processes, networking, builds, storage) through providers.
//!
//! Each provider:
//! - Declares its domain name and supported actions
//! - Specifies required permissions per action
//! - Executes actions within the kernel's security boundary
//! - Returns structured JSON results (never raw stdout)
//! - Generates audit metadata for per-resource logging

use agentos_types::*;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// A managed capability domain that the kernel mediates on behalf of agents.
///
/// Providers handle a family of related actions within a single domain
/// (e.g., all `env.*` operations). The kernel validates permissions, fires
/// policy hooks, and checks the capability broker before calling `execute`.
///
/// # Security Model
///
/// - The kernel validates the agent's `CapabilityToken` before calling `execute`.
/// - Providers may perform additional domain-specific validation (e.g., package
///   allowlist checks, binary allowlist checks).
/// - Every execution produces `audit_metadata` that is merged into the audit entry.
/// - Providers must not bypass the capability system or access resources beyond
///   what the `CapabilityContext` grants.
#[async_trait]
pub trait CapabilityProvider: Send + Sync {
    /// Domain prefix for this provider (e.g., `"env"`, `"proc"`, `"net"`).
    ///
    /// This is used as the namespace for permission resources and audit events.
    /// For example, a provider with domain `"env"` handles actions like
    /// `env.install`, `env.create`, etc.
    fn domain(&self) -> &str;

    /// List of actions this provider supports.
    ///
    /// Each action name is combined with the domain to form the full capability
    /// identifier: `"{domain}.{action}"` (e.g., `"env.install"`).
    fn supported_actions(&self) -> &[&str];

    /// Required permissions for a given action.
    ///
    /// Returns `(resource, PermissionOp)` pairs that the agent's capability token
    /// must satisfy. The kernel checks these before calling `execute`.
    ///
    /// Returns `None` if the action is not supported by this provider.
    fn required_permissions(&self, action: &str) -> Option<Vec<(String, PermissionOp)>>;

    /// Execute a capability action.
    ///
    /// # Preconditions
    ///
    /// Before a provider's `execute` runs, the dispatcher
    /// (`KernelCapabilityDispatcher::dispatch`) has:
    /// 1. Checked `required_permissions(action)` against the request's token
    ///    permissions (static authorization).
    /// 2. Evaluated the dynamic policy engine for this `(domain, action,
    ///    resource)`; a `Deny`/`Escalate` result blocks execution.
    ///
    /// Note: `HookEvent::ToolPre` (interactive approval) is fired by the tool
    /// CALLER — the task executor and the chat path both gate the bridge tool
    /// name through `ToolPre` before reaching dispatch. The dispatcher itself
    /// does not fire ToolPre, so a code path that reaches `dispatch` without
    /// going through a gated tool call would skip approval — keep all dispatch
    /// entry points behind a `ToolPre`-gated tool invocation.
    ///
    /// # Errors
    ///
    /// Returns `AgentOSError` on validation failure, policy violation, or execution error.
    async fn execute(
        &self,
        action: &str,
        params: Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError>;

    /// Human-readable description for agent tool discovery and manuals.
    fn description(&self) -> &str;
}

/// Context passed to every capability provider execution.
///
/// Contains the agent's identity, permissions, and kernel resource references
/// needed for mediated execution. Providers use this to scope their actions
/// to the requesting agent and validate domain-specific constraints.
#[derive(Debug, Clone)]
pub struct CapabilityContext {
    /// The agent requesting the capability.
    pub agent_id: AgentID,
    /// The task that triggered the request.
    pub task_id: TaskID,
    /// Distributed trace identifier for correlation.
    pub trace_id: TraceID,
    /// The kernel data directory. Holds kernel state (vault, audit log, other
    /// agents' homes) — never expose it to agent-run commands; use
    /// [`exec_roots`](Self::exec_roots).
    pub data_dir: PathBuf,
    /// The agent's effective permission set.
    pub permissions: PermissionSet,
    /// Additional directories the agent may access.
    pub workspace_paths: Vec<PathBuf>,
    /// The agent's home (`data_dir/agents/<name>/`).
    pub agent_home: PathBuf,
    /// Workspace grants with `--mode rwx`.
    pub workspace_paths_executable: Vec<PathBuf>,
}

impl CapabilityContext {
    /// Directories agent-controlled commands (`build.*`, `proc.*`) may run in
    /// and write: the agent home, its managed workspaces, and `rwx` grants.
    /// The rest of `data_dir` (vault, audit log, other agents) never is.
    pub fn exec_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![
            self.agent_home.clone(),
            crate::managed_env::agent_workspaces_root(&self.data_dir, &self.agent_id),
        ];
        roots.extend(
            agentos_tools::sandbox_fs::grants_outside(
                &self.workspace_paths_executable,
                &self.data_dir,
            )
            .cloned(),
        );
        roots
    }

    /// The working directory for an agent command: `requested` if it lies
    /// inside [`exec_roots`](Self::exec_roots), else `default`.
    pub fn exec_working_dir(
        &self,
        requested: Option<&str>,
        default: PathBuf,
        resource: &str,
    ) -> Result<PathBuf, AgentOSError> {
        let Some(requested) = requested else {
            return Ok(default);
        };
        let dir = PathBuf::from(requested);
        let roots = self.exec_roots();
        // `starts_with` is lexical: `root/../vault` would pass it.
        let lexically_safe = dir.is_absolute()
            && !dir
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
        // A symlink inside a root (`home/x -> /root`) passes the lexical test;
        // the sandbox would not follow it out, but host-side probes of the dir
        // (ecosystem detection) would.
        let resolves_inside = || match std::fs::canonicalize(&dir) {
            Ok(real) => roots
                .iter()
                .filter_map(|r| std::fs::canonicalize(r).ok())
                .any(|r| real.starts_with(r)),
            Err(_) => true,
        };
        if lexically_safe && roots.iter().any(|r| dir.starts_with(r)) && resolves_inside() {
            return Ok(dir);
        }
        let allowed: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
        Err(AgentOSError::PermissionDenied {
            resource: resource.into(),
            operation: format!(
                "working_dir '{requested}' is outside agent scope (absolute path, no `..`). \
                 Allowed: [{}]; grant a folder with --mode rwx to build or run there",
                allowed.join(", ")
            ),
        })
    }

    /// `allow_network` from the payload. Egress from agent-controlled code is
    /// a capability of its own: asking for it requires `net.outbound:x` (the
    /// KMC name) or `network.outbound:x` (what `shell-exec` and default agent
    /// grants use).
    pub fn exec_network(&self, params: &Value, resource: &str) -> Result<bool, AgentOSError> {
        let wanted = params["allow_network"].as_bool().unwrap_or(false);
        let granted = ["net.outbound", "network.outbound"]
            .iter()
            .any(|r| self.permissions.check(r, PermissionOp::Execute));
        if wanted && !granted {
            return Err(AgentOSError::PermissionDenied {
                resource: "net.outbound".into(),
                operation: format!(
                    "{resource} allow_network=true requires net.outbound:x or network.outbound:x"
                ),
            });
        }
        Ok(wanted)
    }

    /// Sandbox for an agent command: every exec root writable, `HOME` at the
    /// agent home, network as decided by [`exec_network`](Self::exec_network).
    pub fn exec_sandbox(&self, tool: &str, network: bool) -> agentos_tools::sandbox_fs::Sandbox {
        self.exec_roots()
            .into_iter()
            .fold(
                agentos_tools::sandbox_fs::Sandbox::new(tool),
                |sandbox, root| sandbox.bind_rw(root),
            )
            .network(network)
            .env("HOME", &self.agent_home)
    }
}

/// Structured result from a capability provider.
///
/// Returned inside `Ok(...)` when the action executes successfully. Failures
/// should be returned as `Err(AgentOSError)` — the `Result` type carries the
/// success/failure semantics, so there is no separate `success` flag.
///
/// # Audit Events
///
/// The kernel dispatch layer (not the provider) emits:
/// - `CapabilityRequested` before calling `execute`
/// - `CapabilityExecuted` after a successful `execute` return
/// - `CapabilityFailed` after `execute` returns `Err`
/// - Domain-specific events from `audit_metadata` are merged into the entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityResult {
    /// Structured JSON output (provider-specific schema).
    ///
    /// This is what the agent sees. It should be concise, machine-readable,
    /// and directly useful for LLM reasoning — never raw stdout.
    pub output: Value,
    /// Audit metadata — merged into the audit event's `details` field.
    ///
    /// Should include domain-specific fields like package names, process IDs,
    /// network destinations, file paths, etc. for per-resource audit granularity.
    pub audit_metadata: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A mock provider for testing.
    struct MockProvider;

    #[async_trait]
    impl CapabilityProvider for MockProvider {
        fn domain(&self) -> &str {
            "mock"
        }

        fn supported_actions(&self) -> &[&str] {
            &["echo", "fail"]
        }

        fn required_permissions(&self, action: &str) -> Option<Vec<(String, PermissionOp)>> {
            match action {
                "echo" => Some(vec![("mock.echo".to_string(), PermissionOp::Execute)]),
                "fail" => Some(vec![("mock.fail".to_string(), PermissionOp::Execute)]),
                _ => None,
            }
        }

        async fn execute(
            &self,
            action: &str,
            params: Value,
            _context: &CapabilityContext,
        ) -> Result<CapabilityResult, AgentOSError> {
            match action {
                "echo" => Ok(CapabilityResult {
                    output: json!({ "echoed": params }),
                    audit_metadata: json!({ "action": "echo" }),
                }),
                "fail" => Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "mock".into(),
                    reason: "intentional failure".into(),
                }),
                _ => Err(AgentOSError::KernelError {
                    reason: format!("unknown action '{action}'"),
                }),
            }
        }

        fn description(&self) -> &str {
            "Mock provider for testing"
        }
    }

    fn make_context() -> CapabilityContext {
        CapabilityContext {
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
            data_dir: PathBuf::from("/tmp/test-data"),
            permissions: PermissionSet::default(),
            workspace_paths: vec![],
            agent_home: PathBuf::from("/tmp/test-data/agents/test"),
            workspace_paths_executable: vec![],
        }
    }

    #[test]
    fn provider_domain_and_actions() {
        let p = MockProvider;
        assert_eq!(p.domain(), "mock");
        assert_eq!(p.supported_actions(), &["echo", "fail"]);
    }

    #[test]
    fn provider_required_permissions_known_action() {
        let p = MockProvider;
        let perms = p.required_permissions("echo").unwrap();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].0, "mock.echo");
    }

    #[test]
    fn provider_required_permissions_unknown_action() {
        let p = MockProvider;
        assert!(p.required_permissions("nonexistent").is_none());
    }

    #[tokio::test]
    async fn provider_execute_echo() {
        let p = MockProvider;
        let ctx = make_context();
        let result = p
            .execute("echo", json!({"msg": "hello"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result.output["echoed"]["msg"], "hello");
    }

    #[tokio::test]
    async fn provider_execute_fail() {
        let p = MockProvider;
        let ctx = make_context();
        let err = p.execute("fail", json!({}), &ctx).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("intentional failure"));
    }

    #[tokio::test]
    async fn provider_execute_unknown_action() {
        let p = MockProvider;
        let ctx = make_context();
        let err = p.execute("unknown", json!({}), &ctx).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown action"));
    }
}
