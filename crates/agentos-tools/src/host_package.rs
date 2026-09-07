//! `host-package-install` — install host OS packages via system package manager.
//!
//! Risk class: `control_plane` (mandatory user approval every call).
//! Executor: `privileged` (runs OUTSIDE the bwrap sandbox via `pkexec` or a
//! setuid helper).
//!
//! Defense in depth:
//!   1. Manifest declares `risk_class = control_plane` → `ApprovalHook` blocks
//!      execution until a paired user explicitly approves the escalation.
//!   2. Sandbox policy hard-rejects `Privileged` executor unless
//!      `trust_tier = core` AND `risk_class = control_plane`.
//!   3. Tool body validates `package` against an operator-controlled allowlist
//!      BEFORE invoking the privilege escalator — even an approved escalation
//!      cannot install something not in the allowlist.

use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Maximum total wall-clock duration of a host install command.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

const TOOL_NAME: &str = "host-package-install";

#[inline]
fn tef(reason: impl Into<String>) -> AgentOSError {
    AgentOSError::ToolExecutionFailed {
        tool_name: TOOL_NAME.into(),
        reason: reason.into(),
    }
}

/// Privilege escalation strategies available at boot time.
#[derive(Debug, Clone, Default)]
pub enum EscalatorPolicy {
    /// Detect at runtime — prefer `pkexec`, fall back to setuid helper, fail otherwise.
    #[default]
    Auto,
    /// Force `pkexec` only.
    Pkexec,
    /// Force the setuid helper at the given path.
    Helper(PathBuf),
    /// Disable the tool entirely (returns error on every call).
    None,
}

/// Trait for invoking commands with elevated privileges. Injected into
/// `HostPackageInstallTool` so tests can substitute a mock that does not
/// require root.
#[async_trait]
pub trait PrivilegeEscalator: Send + Sync {
    async fn run(
        &self,
        argv: &[String],
        timeout: Duration,
    ) -> Result<EscalatorOutput, AgentOSError>;

    fn label(&self) -> &'static str;
}

#[derive(Debug)]
pub struct EscalatorOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Real `pkexec` invocation — pops a polkit auth prompt on Linux desktops.
pub struct PkexecEscalator;

#[async_trait]
impl PrivilegeEscalator for PkexecEscalator {
    async fn run(
        &self,
        argv: &[String],
        timeout: Duration,
    ) -> Result<EscalatorOutput, AgentOSError> {
        let mut cmd = tokio::process::Command::new("pkexec");
        cmd.args(argv);
        run_with_timeout(cmd, timeout).await
    }

    fn label(&self) -> &'static str {
        "pkexec"
    }
}

/// Setuid helper invocation — for headless servers where polkit is unavailable.
/// The helper binary itself MUST validate argv against its own allowlist.
pub struct HelperEscalator {
    helper_path: PathBuf,
}

impl HelperEscalator {
    pub fn new(helper_path: PathBuf) -> Self {
        Self { helper_path }
    }
}

#[async_trait]
impl PrivilegeEscalator for HelperEscalator {
    async fn run(
        &self,
        argv: &[String],
        timeout: Duration,
    ) -> Result<EscalatorOutput, AgentOSError> {
        let mut cmd = tokio::process::Command::new(&self.helper_path);
        cmd.args(argv);
        run_with_timeout(cmd, timeout).await
    }

    fn label(&self) -> &'static str {
        "helper"
    }
}

async fn run_with_timeout(
    mut cmd: tokio::process::Command,
    timeout: Duration,
) -> Result<EscalatorOutput, AgentOSError> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Spawn explicitly so the kill-on-drop semantic is visible in the source.
    // `Child` carries `kill_on_drop = true` from the `Command`, so when the
    // `wait_with_output` future is dropped on timeout, the child receives
    // SIGKILL via tokio's internal `Drop` impl on `Child`.
    let child = cmd.spawn().map_err(|e| tef(format!("spawn failed: {e}")))?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => Ok(EscalatorOutput {
            exit_code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).to_string(),
            stderr: String::from_utf8_lossy(&o.stderr).to_string(),
        }),
        Ok(Err(e)) => Err(tef(format!("wait failed: {e}"))),
        Err(_) => Err(tef(format!(
            "host-package-install timed out after {}s",
            timeout.as_secs()
        ))),
    }
}

/// Default setuid helper path used by `EscalatorPolicy::Auto` when `pkexec`
/// is absent. Operators can override via `[tools.host_package].helper_path`
/// + `privilege_escalator = "helper"` for a custom path.
pub const DEFAULT_HELPER_PATH: &str = "/usr/local/libexec/agentos-pkg-helper";

/// Resolve `EscalatorPolicy` to a concrete implementation, probing the
/// runtime for available binaries. Returns `None` only when `policy = None`
/// or no escalator is available on the host.
pub fn resolve_escalator(policy: &EscalatorPolicy) -> Option<Arc<dyn PrivilegeEscalator>> {
    match policy {
        EscalatorPolicy::None => None,
        EscalatorPolicy::Pkexec => binary_in_path("pkexec")
            .then(|| Arc::new(PkexecEscalator) as Arc<dyn PrivilegeEscalator>),
        EscalatorPolicy::Helper(path) => path
            .exists()
            .then(|| Arc::new(HelperEscalator::new(path.clone())) as Arc<dyn PrivilegeEscalator>),
        EscalatorPolicy::Auto => {
            if binary_in_path("pkexec") {
                Some(Arc::new(PkexecEscalator))
            } else {
                // Fall back to the default setuid helper for headless servers
                // where polkit is not installed.
                let helper = PathBuf::from(DEFAULT_HELPER_PATH);
                helper
                    .exists()
                    .then(|| Arc::new(HelperEscalator::new(helper)) as Arc<dyn PrivilegeEscalator>)
            }
        }
    }
}

fn binary_in_path(name: &str) -> bool {
    resolve_binary(name).is_some()
}

/// Resolve `name` against `$PATH` and return the first absolute path that
/// exists, is a regular file, and has at least one executable bit set.
fn resolve_binary(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = candidate.metadata() {
                if meta.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
        }
        return Some(candidate);
    }
    None
}

/// Validate package name: alphanumeric + `-`, `_`, `.`, `+`. No paths, spaces,
/// or shell metacharacters. Length cap 128.
fn validate_package_name(name: &str) -> Result<(), AgentOSError> {
    if name.is_empty() || name.len() > 128 {
        return Err(AgentOSError::SchemaValidation(format!(
            "host-package-install: invalid package name '{name}' (length)"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
    {
        return Err(AgentOSError::SchemaValidation(format!(
            "host-package-install: invalid package name '{name}' (character)"
        )));
    }
    Ok(())
}

/// Validate version constraint: alphanumeric + common version syntax (`-`, `.`,
/// `_`, `+`, `~`, `:`, `=`, `>`, `<`). No spaces or shell metas. Length 64.
fn validate_version(ver: &str) -> Result<(), AgentOSError> {
    if ver.is_empty() || ver.len() > 64 {
        return Err(AgentOSError::SchemaValidation(format!(
            "host-package-install: invalid version '{ver}' (length)"
        )));
    }
    if !ver.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '-' | '_' | '.' | '+' | '~' | ':' | '=' | '>' | '<')
    }) {
        return Err(AgentOSError::SchemaValidation(format!(
            "host-package-install: invalid version '{ver}' (character)"
        )));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct HostPackageInstallIntent {
    package: String,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Serialize)]
struct HostPackageInstallResult {
    installed: bool,
    manager: String,
    /// Absolute resolved path to the manager binary at detection time.
    /// Captured for audit determinism (the privilege escalator may resolve
    /// PATH differently across the privilege boundary).
    manager_path: String,
    package: String,
    version: Option<String>,
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u64,
    escalator: String,
    /// Set when the tool refused to invoke the privilege escalator
    /// (allowlist miss, no escalator configured, no manager detected).
    /// `None` when execution actually reached the escalator.
    /// Stable enum-like values: "not_in_allowlist" | "no_privilege_escalator" |
    /// "no_manager".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    denial_reason: Option<String>,
}

/// Atomic snapshot of the operator-controlled allowlist + manager
/// priority list. Stored under a single `RwLock` so a hot-reload writes
/// both fields together (review finding I2 — separate locks left a
/// torn-window where an in-flight `execute()` could see a new allowlist
/// against an old managers list).
#[derive(Debug, Clone, Default)]
pub struct HostPackageSnapshot {
    pub allowlist: Vec<String>,
    pub managers: Vec<String>,
}

/// Hot-reloadable handle to the operator-controlled `host-package-install`
/// policy. The kernel keeps a clone (cheap — `Arc` clone) and rewrites
/// the inner snapshot whenever `[tools.host_package]` changes on disk
/// (via `ConfigWatcher`). Revocations take effect on the next call —
/// in-flight calls finish under their pre-reload view.
#[derive(Clone, Default)]
pub struct HostPackagePolicy {
    inner: Arc<RwLock<HostPackageSnapshot>>,
}

impl HostPackagePolicy {
    pub fn new(allowlist: Vec<String>, managers: Vec<String>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HostPackageSnapshot {
                allowlist,
                managers,
            })),
        }
    }

    /// Atomically replace the snapshot. Returns the previous snapshot so
    /// the caller can compute a by-name diff for audit (review finding I3).
    pub async fn replace(
        &self,
        allowlist: Vec<String>,
        managers: Vec<String>,
    ) -> HostPackageSnapshot {
        let mut guard = self.inner.write().await;
        std::mem::replace(
            &mut *guard,
            HostPackageSnapshot {
                allowlist,
                managers,
            },
        )
    }

    pub async fn snapshot(&self) -> HostPackageSnapshot {
        self.inner.read().await.clone()
    }
}

pub struct HostPackageInstallTool {
    policy: HostPackagePolicy,
    escalator: Option<Arc<dyn PrivilegeEscalator>>,
}

impl HostPackageInstallTool {
    pub fn new(
        allowlist: Vec<String>,
        managers: Vec<String>,
        escalator: Option<Arc<dyn PrivilegeEscalator>>,
    ) -> Self {
        Self {
            policy: HostPackagePolicy::new(allowlist, managers),
            escalator,
        }
    }

    /// Construct from a shared `HostPackagePolicy` handle. Use this when
    /// the kernel needs to retain a write handle for hot-reload.
    pub fn with_policy(
        policy: HostPackagePolicy,
        escalator: Option<Arc<dyn PrivilegeEscalator>>,
    ) -> Self {
        Self { policy, escalator }
    }

    /// Expose the policy handle so the kernel can wire it into the
    /// `ConfigWatcher` reload path.
    pub fn policy(&self) -> HostPackagePolicy {
        self.policy.clone()
    }

    /// Return the first allowlisted manager binary on PATH along with its
    /// absolute resolved path. Capturing the absolute path at detection time
    /// keeps audit logs deterministic and avoids any TOCTOU window between
    /// `detect_manager` and the privilege escalator's eventual exec.
    async fn detect_manager_in(&self, snapshot: &HostPackageSnapshot) -> Option<(String, PathBuf)> {
        for mgr in &snapshot.managers {
            if let Some(abs) = resolve_binary(mgr) {
                return Some((mgr.clone(), abs));
            }
        }
        None
    }

    /// Build the argv vector for the privilege escalator. `mgr_path` is the
    /// absolute resolved path captured by `detect_manager`; the first slot
    /// of the returned argv is that absolute path so the escalator does not
    /// re-resolve PATH inside the privileged child.
    fn build_install_argv(
        mgr: &str,
        mgr_path: &Path,
        pkg: &str,
        ver: Option<&str>,
    ) -> Result<Vec<String>, AgentOSError> {
        let mgr_arg = mgr_path.to_string_lossy().to_string();
        match mgr {
            "apt-get" => {
                let target = match ver {
                    Some(v) => format!("{pkg}={v}"),
                    None => pkg.to_string(),
                };
                Ok(vec![
                    mgr_arg,
                    "install".into(),
                    "-y".into(),
                    "--no-install-recommends".into(),
                    target,
                ])
            }
            "dnf" | "yum" => {
                let target = match ver {
                    Some(v) => format!("{pkg}-{v}"),
                    None => pkg.to_string(),
                };
                Ok(vec![mgr_arg, "install".into(), "-y".into(), target])
            }
            "pacman" => Ok(vec![mgr_arg, "-S".into(), "--noconfirm".into(), pkg.into()]),
            "zypper" => Ok(vec![
                mgr_arg,
                "--non-interactive".into(),
                "install".into(),
                pkg.into(),
            ]),
            "apk" => Ok(vec![mgr_arg, "add".into(), pkg.into()]),
            "brew" => Ok(vec![mgr_arg, "install".into(), pkg.into()]),
            other => Err(tef(format!("unsupported package manager '{other}'"))),
        }
    }
}

#[async_trait]
impl AgentTool for HostPackageInstallTool {
    fn name(&self) -> &str {
        "host-package-install"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![
            ("system.package".into(), PermissionOp::Execute),
            ("net.outbound".into(), PermissionOp::Execute),
        ]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let intent: HostPackageInstallIntent = serde_json::from_value(payload).map_err(|e| {
            AgentOSError::SchemaValidation(format!("host-package-install: bad input: {e}"))
        })?;

        validate_package_name(&intent.package)?;
        if let Some(ref v) = intent.version {
            validate_version(v)?;
        }

        // Take ONE atomic snapshot of allowlist + managers so the rest of
        // execute() sees a consistent view, even if `ConfigWatcher` reloads
        // the policy mid-call.
        let snapshot = self.policy.snapshot().await;

        // Pre-flight checks (allowlist, escalator, manager). On failure we
        // emit a structured `installed = false` result rather than `Err`,
        // so that the kernel's `AuditHook::ToolPost` path observes a typed
        // `HostPackageInstallDenied` audit event with the rejection reason.
        // Returning `Err` here would leave only a generic
        // `ToolExecutionCompleted` entry which security operators cannot
        // distinguish from a true infrastructure failure (review finding I1).
        let denial: Option<&'static str> =
            if !snapshot.allowlist.iter().any(|p| p == &intent.package) {
                Some("not_in_allowlist")
            } else if self.escalator.is_none() {
                Some("no_privilege_escalator")
            } else {
                None
            };
        if let Some(reason) = denial {
            let stderr = match reason {
                "not_in_allowlist" => format!(
                    "package '{}' is not in the configured allowlist",
                    intent.package
                ),
                "no_privilege_escalator" => "no privilege escalator configured (install pkexec or \
                     set [tools.host_package].helper_path)"
                    .to_string(),
                _ => reason.into(),
            };
            return serde_json::to_value(HostPackageInstallResult {
                installed: false,
                manager: String::new(),
                manager_path: String::new(),
                package: intent.package,
                version: intent.version,
                exit_code: -1,
                stdout: String::new(),
                stderr,
                duration_ms: 0,
                escalator: String::new(),
                denial_reason: Some(reason.into()),
            })
            .map_err(|e| tef(format!("serialize denial: {e}")));
        }

        // Safe to unwrap: we just verified `escalator.is_some()` above and
        // we hold no `&mut self` between the check and use.
        let escalator = self.escalator.as_ref().expect("escalator presence checked");

        let (mgr, mgr_path) = match self.detect_manager_in(&snapshot).await {
            Some(v) => v,
            None => {
                return serde_json::to_value(HostPackageInstallResult {
                    installed: false,
                    manager: String::new(),
                    manager_path: String::new(),
                    package: intent.package,
                    version: intent.version,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: "no allowlisted package manager found on host".into(),
                    duration_ms: 0,
                    escalator: escalator.label().into(),
                    denial_reason: Some("no_manager".into()),
                })
                .map_err(|e| tef(format!("serialize denial: {e}")));
            }
        };

        let argv =
            Self::build_install_argv(&mgr, &mgr_path, &intent.package, intent.version.as_deref())?;

        let start = std::time::Instant::now();
        let output = escalator.run(&argv, INSTALL_TIMEOUT).await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        let installed = output.exit_code == 0;
        let result = HostPackageInstallResult {
            installed,
            manager: mgr,
            manager_path: mgr_path.to_string_lossy().to_string(),
            package: intent.package,
            version: intent.version,
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            duration_ms,
            escalator: escalator.label().to_string(),
            denial_reason: None,
        };

        serde_json::to_value(result).map_err(|e| tef(format!("serialize result: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;
    use std::sync::Mutex;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
        }
    }

    struct MockEscalator {
        captured: Mutex<Vec<Vec<String>>>,
        exit_code: i32,
        stdout: String,
        stderr: String,
    }

    impl MockEscalator {
        fn ok() -> Arc<Self> {
            Arc::new(Self {
                captured: Mutex::new(vec![]),
                exit_code: 0,
                stdout: "Setting up python3 ...\n".into(),
                stderr: String::new(),
            })
        }
    }

    #[async_trait]
    impl PrivilegeEscalator for MockEscalator {
        async fn run(
            &self,
            argv: &[String],
            _timeout: Duration,
        ) -> Result<EscalatorOutput, AgentOSError> {
            self.captured.lock().unwrap().push(argv.to_vec());
            Ok(EscalatorOutput {
                exit_code: self.exit_code,
                stdout: self.stdout.clone(),
                stderr: self.stderr.clone(),
            })
        }
        fn label(&self) -> &'static str {
            "mock"
        }
    }

    #[test]
    fn build_apt_argv_with_version() {
        let argv = HostPackageInstallTool::build_install_argv(
            "apt-get",
            Path::new("/usr/bin/apt-get"),
            "python3",
            Some("3.12.1"),
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "/usr/bin/apt-get",
                "install",
                "-y",
                "--no-install-recommends",
                "python3=3.12.1"
            ]
        );
    }

    #[test]
    fn build_pacman_argv_no_version() {
        let argv = HostPackageInstallTool::build_install_argv(
            "pacman",
            Path::new("/usr/bin/pacman"),
            "python",
            None,
        )
        .unwrap();
        assert_eq!(argv, vec!["/usr/bin/pacman", "-S", "--noconfirm", "python"]);
    }

    #[test]
    fn build_apk_argv() {
        let argv = HostPackageInstallTool::build_install_argv(
            "apk",
            Path::new("/sbin/apk"),
            "py3-pip",
            None,
        )
        .unwrap();
        assert_eq!(argv, vec!["/sbin/apk", "add", "py3-pip"]);
    }

    #[test]
    fn build_argv_unsupported_mgr() {
        let err = HostPackageInstallTool::build_install_argv(
            "snake-oil",
            Path::new("/bogus"),
            "python",
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
    }

    #[test]
    fn validate_package_name_accepts_normal() {
        assert!(validate_package_name("python3").is_ok());
        assert!(validate_package_name("python3-pip").is_ok());
        assert!(validate_package_name("ca-certificates").is_ok());
        assert!(validate_package_name("g++").is_ok());
    }

    #[test]
    fn validate_package_name_rejects_shell_metas() {
        assert!(validate_package_name("python; rm -rf /").is_err());
        assert!(validate_package_name("python && evil").is_err());
        assert!(validate_package_name("../etc/passwd").is_err());
        assert!(validate_package_name("foo bar").is_err());
        assert!(validate_package_name("").is_err());
        assert!(validate_package_name(&"x".repeat(200)).is_err());
    }

    #[test]
    fn validate_version_accepts_common() {
        assert!(validate_version("3.12.1").is_ok());
        assert!(validate_version(">=2.0").is_ok());
        assert!(validate_version("1:2.3-4").is_ok());
    }

    #[test]
    fn validate_version_rejects_meta() {
        assert!(validate_version("1.0; evil").is_err());
        assert!(validate_version("1.0 OR 1=1").is_err());
    }

    #[tokio::test]
    async fn execute_rejects_package_not_in_allowlist() {
        let tool = HostPackageInstallTool::new(
            vec!["python3".into()],
            vec!["apt-get".into()],
            Some(MockEscalator::ok()),
        );
        // Allowlist denial now flows as Ok(installed=false) so the AuditHook
        // post-tool path emits a typed `HostPackageInstallDenied` entry.
        let value = tool
            .execute(serde_json::json!({"package": "nginx"}), ctx())
            .await
            .unwrap();
        assert_eq!(value["installed"], false);
        assert_eq!(value["denial_reason"], "not_in_allowlist");
        assert!(value["stderr"]
            .as_str()
            .unwrap()
            .contains("not in the configured allowlist"));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_package_name_before_allowlist() {
        let tool = HostPackageInstallTool::new(
            vec!["python; evil".into()], // even if a malicious operator put this in allowlist
            vec!["apt-get".into()],
            Some(MockEscalator::ok()),
        );
        // Schema validation still returns Err — package name validation
        // happens before allowlist check and is a true input error, not a
        // policy denial.
        let err = tool
            .execute(serde_json::json!({"package": "python; evil"}), ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    }

    #[tokio::test]
    async fn execute_fails_when_no_escalator() {
        let tool =
            HostPackageInstallTool::new(vec!["python3".into()], vec!["apt-get".into()], None);
        // No-escalator denial also flows as Ok(installed=false) so the
        // post-hook can emit `HostPackageInstallDenied` instead of a
        // generic ToolExecutionFailed entry.
        let value = tool
            .execute(serde_json::json!({"package": "python3"}), ctx())
            .await
            .unwrap();
        assert_eq!(value["installed"], false);
        assert_eq!(value["denial_reason"], "no_privilege_escalator");
        assert!(value["stderr"]
            .as_str()
            .unwrap()
            .contains("no privilege escalator"));
    }

    #[tokio::test]
    async fn resolve_escalator_none_returns_none() {
        assert!(resolve_escalator(&EscalatorPolicy::None).is_none());
    }

    #[tokio::test]
    async fn resolve_helper_missing_returns_none() {
        let bogus = PathBuf::from("/no/such/agentos-pkg-helper");
        assert!(resolve_escalator(&EscalatorPolicy::Helper(bogus)).is_none());
    }
}
