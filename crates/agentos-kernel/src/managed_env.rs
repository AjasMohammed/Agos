//! Managed Environments capability provider (`env.*`).
//!
//! Allows agents to create isolated workspaces, install packages, and manage
//! dependencies — all mediated by the kernel with allowlist policy and
//! per-agent isolation.
//!
//! Workspaces are scoped per-agent at `{data_dir}/workspaces/{workspace_name}/`.
//! Venv creation and package installation run in a bwrap [`Sandbox`] that sees
//! only the workspace (writable) and system dirs (read-only), with a cleared
//! environment; installs get network, creation does not. No bwrap, no install.

use crate::capability_provider::{CapabilityContext, CapabilityProvider, CapabilityResult};
use agentos_tools::sandbox_fs::Sandbox;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Supported package ecosystems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ecosystem {
    Python,
    #[serde(alias = "nodejs")]
    NodeJs,
    Rust,
    /// System package manager (apt/dnf) — requires elevated policy.
    System,
    /// No package manager, just a directory.
    Generic,
}

impl Ecosystem {
    fn from_str_loose(s: &str) -> Result<Self, AgentOSError> {
        match s.to_ascii_lowercase().as_str() {
            "python" | "py" => Ok(Self::Python),
            "nodejs" | "node" | "npm" => Ok(Self::NodeJs),
            "rust" | "cargo" => Ok(Self::Rust),
            "system" | "apt" | "dnf" => Ok(Self::System),
            "generic" | "none" => Ok(Self::Generic),
            other => Err(AgentOSError::SchemaValidation(format!(
                "unknown ecosystem '{other}': expected python, nodejs, rust, system, or generic"
            ))),
        }
    }
}

/// Record of an installed package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub ecosystem: Ecosystem,
    pub installed_at: chrono::DateTime<chrono::Utc>,
}

/// An isolated workspace for an agent's project environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedWorkspace {
    pub name: String,
    pub agent_id: agentos_types::AgentID,
    pub root_path: PathBuf,
    pub ecosystem: Ecosystem,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub packages_installed: Vec<InstalledPackage>,
}

// ---------------------------------------------------------------------------
// Environment configuration (loaded from kernel config)
// ---------------------------------------------------------------------------

/// Configuration for the managed environments capability.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnvConfig {
    /// Maximum workspace disk usage per agent in bytes.
    #[serde(default = "default_quota")]
    pub default_quota_bytes: u64,
    /// Package policy per ecosystem: "curated", "open", or "locked".
    #[serde(default = "default_curated")]
    pub python_policy: String,
    #[serde(default = "default_curated")]
    pub nodejs_policy: String,
    #[serde(default = "default_curated")]
    pub rust_policy: String,
    #[serde(default = "default_locked")]
    pub system_policy: String,
    /// Network timeout for package installation (seconds).
    #[serde(default = "default_install_timeout")]
    pub install_timeout_secs: u64,
}

fn default_quota() -> u64 {
    2_147_483_648 // 2 GB
}
fn default_curated() -> String {
    "curated".into()
}
fn default_locked() -> String {
    "locked".into()
}
fn default_install_timeout() -> u64 {
    120
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            default_quota_bytes: default_quota(),
            python_policy: default_curated(),
            nodejs_policy: default_curated(),
            rust_policy: default_curated(),
            system_policy: default_locked(),
            install_timeout_secs: default_install_timeout(),
        }
    }
}

/// Package allowlist for a specific ecosystem.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PackageAllowlist {
    #[serde(default)]
    pub packages: Vec<String>,
}

// ---------------------------------------------------------------------------
// Workspace name validation
// ---------------------------------------------------------------------------

/// Validate workspace name: must be 1-64 chars, alphanumeric + hyphens + underscores.
fn validate_workspace_name(name: &str) -> Result<(), AgentOSError> {
    const MAX_LEN: usize = 64;
    if name.is_empty()
        || name.len() > MAX_LEN
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AgentOSError::SchemaValidation(format!(
            "invalid workspace name '{name}': must be 1-{MAX_LEN} chars, \
             ASCII alphanumeric with hyphens/underscores"
        )));
    }
    Ok(())
}

/// Validate version constraint: must be 1-64 chars, semver-like characters only.
/// Rejects shell metacharacters to prevent command injection.
fn validate_version(version: &str) -> Result<(), AgentOSError> {
    const MAX_LEN: usize = 64;
    if version.is_empty()
        || version.len() > MAX_LEN
        || !version.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || c == '.'
                || c == '-'
                || c == '+'
                || c == '='
                || c == '>'
                || c == '<'
                || c == '~'
                || c == '^'
                || c == '*'
        })
    {
        return Err(AgentOSError::SchemaValidation(format!(
            "invalid version constraint '{version}': must be 1-{MAX_LEN} chars, \
             semver characters only (no shell metacharacters)"
        )));
    }
    Ok(())
}

/// Validate package name: must be 1-128 chars, alphanumeric + hyphens + underscores + dots.
fn validate_package_name(name: &str) -> Result<(), AgentOSError> {
    const MAX_LEN: usize = 128;
    if name.is_empty()
        || name.len() > MAX_LEN
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(AgentOSError::SchemaValidation(format!(
            "invalid package name '{name}': must be 1-{MAX_LEN} chars, \
             ASCII alphanumeric with hyphens/underscores/dots"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Workspace lookup trait (used by BuildProvider / ProcessProvider)
// ---------------------------------------------------------------------------

/// Snapshot of a managed workspace, returned to other capability providers
/// that need to launch processes inside it.
#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub root: PathBuf,
    pub ecosystem: Ecosystem,
}

/// Pluggable lookup so non-`env` providers (build, proc) can resolve a
/// workspace name to its path + ecosystem without depending on `EnvProvider`'s
/// concrete type.
#[async_trait]
pub trait WorkspaceResolver: Send + Sync {
    async fn resolve(&self, agent_id: agentos_types::AgentID, name: &str) -> Option<WorkspaceInfo>;
}

/// `{data_dir}/workspaces/{agent_id}/` — every managed workspace of one agent.
pub fn agent_workspaces_root(data_dir: &Path, agent_id: &agentos_types::AgentID) -> PathBuf {
    data_dir.join("workspaces").join(agent_id.to_string())
}

/// Workspace-local bin dirs that exist, in lookup order: `{ws}/venv/bin` →
/// `{ws}/node_modules/.bin` → `{ws}/bin`. Missing ones are skipped so a Rust
/// workspace doesn't get a phantom `venv/bin` entry.
pub fn workspace_bin_dirs(ws: &WorkspaceInfo) -> Vec<PathBuf> {
    [
        ws.root.join("venv").join("bin"),
        ws.root.join("node_modules").join(".bin"),
        ws.root.join("bin"),
    ]
    .into_iter()
    .filter(|d| d.is_dir())
    .collect()
}

/// Env vars (besides `PATH`) a child needs to see the workspace's packages.
/// Only ever applied inside a [`Sandbox`], which starts from a cleared
/// environment — the kernel's own holds every provider API key.
pub fn activated_env(ws: &WorkspaceInfo) -> Vec<(String, String)> {
    let mut env = vec![
        // Package-manager caches (`~/.cache/pip`, `~/.npm`, `~/.cargo`) land
        // in the workspace, the only writable place that persists.
        ("HOME".to_string(), ws.root.to_string_lossy().into_owned()),
        ("LC_ALL".to_string(), "C.UTF-8".to_string()),
    ];
    if matches!(ws.ecosystem, Ecosystem::Python) {
        env.push((
            "VIRTUAL_ENV".to_string(),
            ws.root.join("venv").to_string_lossy().into_owned(),
        ));
    }
    env
}

/// Run inside `ws`: its bins first on `PATH`, its env set.
///
/// SECURITY: does not bind `ws.root`. The caller binds the agent's
/// [`agent_workspaces_root`] instead — a bind of `ws.root` itself would sit
/// under that writable directory, where a sandboxed command can swap it for a
/// symlink that bwrap then resolves on the host.
pub fn activate(mut sandbox: Sandbox, ws: &WorkspaceInfo) -> Result<Sandbox, AgentOSError> {
    if ws.ecosystem == Ecosystem::Python && !venv_usable_in_sandbox(&ws.root) {
        return Err(AgentOSError::KernelError {
            reason: "this Python workspace's venv cannot run in the sandbox (a stub, or created \
                     for a host interpreter under /home before sandboxing); recreate it with \
                     env-destroy + env-create"
                .into(),
        });
    }
    for dir in workspace_bin_dirs(ws) {
        sandbox = sandbox.path_prepend(dir);
    }
    for (k, v) in activated_env(ws) {
        sandbox = sandbox.env(k, v);
    }
    Ok(sandbox)
}

/// Whether `venv/bin/python` resolves to an interpreter the sandbox can see.
fn venv_usable_in_sandbox(root: &Path) -> bool {
    std::fs::canonicalize(root.join("venv").join("bin").join("python")).is_ok_and(|p| {
        ["/usr", "/bin", "/lib", "/lib64", "/sbin"]
            .iter()
            .any(|d| p.starts_with(d))
    })
}

/// `create_dir` that accepts an existing real directory but never follows or
/// accepts a symlink — workspace dirs sit in agent-writable space, and
/// `create_dir_all` through a planted link would create folders anywhere the
/// kernel user can write.
fn ensure_real_dir(path: &Path) -> Result<(), AgentOSError> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::symlink_metadata(path) {
                Ok(m) if m.is_dir() => Ok(()),
                _ => Err(AgentOSError::KernelError {
                    reason: format!(
                        "workspace path '{}' exists and is not a plain directory",
                        path.display()
                    ),
                }),
            }
        }
        Err(e) => Err(AgentOSError::KernelError {
            reason: format!("failed to create '{}': {e}", path.display()),
        }),
    }
}

// ---------------------------------------------------------------------------
// EnvProvider
// ---------------------------------------------------------------------------

/// Managed environments capability provider.
///
/// Manages per-agent workspaces for package installation and dependency management.
/// All workspaces live under `{data_dir}/workspaces/` and are isolated per-agent.
pub struct EnvProvider {
    /// Per-agent workspace state. Key: (agent_id, workspace_name).
    workspaces: Arc<RwLock<HashMap<(agentos_types::AgentID, String), ManagedWorkspace>>>,
    /// Env configuration.
    config: EnvConfig,
    /// Package allowlists per ecosystem.
    allowlists: HashMap<Ecosystem, PackageAllowlist>,
    /// Optional SQLite-backed persistence. When set, every create / install /
    /// destroy is written through; absent means in-memory only (tests).
    store: Option<Arc<crate::workspace_store::WorkspaceStore>>,
}

impl EnvProvider {
    /// Create a new EnvProvider with the given configuration and allowlists.
    pub fn new(config: EnvConfig, allowlists: HashMap<Ecosystem, PackageAllowlist>) -> Self {
        Self {
            workspaces: Arc::new(RwLock::new(HashMap::new())),
            config,
            allowlists,
            store: None,
        }
    }

    /// Create with default config and empty allowlists.
    pub fn with_defaults() -> Self {
        Self::new(EnvConfig::default(), HashMap::new())
    }

    /// Build an `EnvProvider` from the kernel `EnvSettings` block.
    ///
    /// Translates the flat config struct into the provider's internal
    /// `EnvConfig` + `HashMap<Ecosystem, PackageAllowlist>` shape. Empty
    /// allowlists are omitted so the curated check fails fast with a clear
    /// "not on the curated allowlist" error.
    pub fn from_config(settings: &crate::config::EnvSettings) -> Self {
        for (eco, raw) in [
            ("python_policy", &settings.python_policy),
            ("nodejs_policy", &settings.nodejs_policy),
            ("rust_policy", &settings.rust_policy),
            ("system_policy", &settings.system_policy),
        ] {
            if !matches!(raw.as_str(), "curated" | "open" | "locked") {
                tracing::warn!(
                    field = eco,
                    value = %raw,
                    "unrecognized env policy value; falling back to 'curated' (deny by default). \
                     Valid values: \"curated\", \"open\", \"locked\""
                );
            }
        }
        let cfg = EnvConfig {
            default_quota_bytes: settings.default_quota_bytes,
            python_policy: settings.python_policy.clone(),
            nodejs_policy: settings.nodejs_policy.clone(),
            rust_policy: settings.rust_policy.clone(),
            system_policy: settings.system_policy.clone(),
            install_timeout_secs: settings.install_timeout_secs,
        };
        let mut allowlists = HashMap::new();
        if !settings.python_allowlist.is_empty() {
            allowlists.insert(
                Ecosystem::Python,
                PackageAllowlist {
                    packages: settings.python_allowlist.clone(),
                },
            );
        }
        if !settings.nodejs_allowlist.is_empty() {
            allowlists.insert(
                Ecosystem::NodeJs,
                PackageAllowlist {
                    packages: settings.nodejs_allowlist.clone(),
                },
            );
        }
        if !settings.rust_allowlist.is_empty() {
            allowlists.insert(
                Ecosystem::Rust,
                PackageAllowlist {
                    packages: settings.rust_allowlist.clone(),
                },
            );
        }
        Self::new(cfg, allowlists)
    }

    /// Like `from_config`, but attaches a `WorkspaceStore` and pre-loads the
    /// in-memory map from the DB. This is the constructor the kernel uses at
    /// boot so workspaces survive restarts.
    pub async fn from_config_with_store(
        settings: &crate::config::EnvSettings,
        store: Arc<crate::workspace_store::WorkspaceStore>,
    ) -> Result<Self, AgentOSError> {
        let mut provider = Self::from_config(settings);
        provider.store = Some(store.clone());

        let rows = store
            .load_all()
            .await
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("failed to load workspaces from store: {e}"),
            })?;
        let mut map = HashMap::new();
        for ws in rows {
            map.insert((ws.agent_id, ws.name.clone()), ws);
        }
        // No tokio lock held yet; replace the empty map wholesale.
        *provider.workspaces.write().await = map;
        Ok(provider)
    }

    /// Compare on-disk `{data_dir}/workspaces/<agent>/<name>/` directories
    /// against the in-memory map and emit warnings for either direction of
    /// drift. Warn-only — never destructive — so operators can investigate
    /// before any auto-clean is contemplated.
    pub async fn warn_on_disk_drift(&self, data_dir: &Path) {
        let root = data_dir.join("workspaces");
        if !root.is_dir() {
            return;
        }
        let ws_map = self.workspaces.read().await;
        let known: std::collections::HashSet<(String, String)> = ws_map
            .keys()
            .map(|(a, n)| (a.to_string(), n.clone()))
            .collect();
        // Forward direction: directories present on disk but not in DB.
        let mut on_disk: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        match std::fs::read_dir(&root) {
            Ok(agent_dirs) => {
                for agent_entry in agent_dirs.flatten() {
                    let agent_str = match agent_entry.file_name().into_string() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let agent_path = agent_entry.path();
                    if !agent_path.is_dir() {
                        continue;
                    }
                    if let Ok(ws_dirs) = std::fs::read_dir(&agent_path) {
                        for ws_entry in ws_dirs.flatten() {
                            if let Ok(ws_name) = ws_entry.file_name().into_string() {
                                if ws_entry.path().is_dir() {
                                    on_disk.insert((agent_str.clone(), ws_name));
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %root.display(), "failed to scan workspaces dir for drift");
                return;
            }
        }
        for orphan in on_disk.difference(&known) {
            tracing::warn!(
                agent_id = %orphan.0,
                workspace = %orphan.1,
                "workspace directory exists on disk but has no row in workspaces.db (orphan — investigate before cleaning)"
            );
        }
        for missing in known.difference(&on_disk) {
            tracing::warn!(
                agent_id = %missing.0,
                workspace = %missing.1,
                "workspaces.db row exists but workspace directory is missing on disk (manual delete?)"
            );
        }
    }

    /// Resolve workspace root path for an agent.
    /// Includes agent_id in the path to prevent cross-agent filesystem collision.
    fn workspace_path(
        data_dir: &Path,
        agent_id: &agentos_types::AgentID,
        workspace_name: &str,
    ) -> PathBuf {
        agent_workspaces_root(data_dir, agent_id).join(workspace_name)
    }

    /// `python3 -m venv {root}/venv` in a network-less sandbox that binds the
    /// agent's workspaces `scope` (see [`activate`] for why not `root`).
    async fn create_venv(scope: &Path, root: &Path) -> Result<(), String> {
        let mut cmd = Sandbox::new("env-create")
            .bind_rw(scope)
            .env("HOME", root)
            .command(root, "python3")
            .await
            .map_err(|e| e.to_string())?;
        cmd.args(["-m", "venv"]).arg(root.join("venv"));
        let out = tokio::time::timeout(std::time::Duration::from_secs(120), cmd.output())
            .await
            .map_err(|_| "python3 -m venv timed out".to_string())?
            .map_err(|e| format!("failed to run python3: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(200)
                .collect())
        }
    }

    /// Check whether a package is allowed by policy.
    fn check_package_allowed(
        &self,
        ecosystem: Ecosystem,
        package_name: &str,
    ) -> Result<(), AgentOSError> {
        let policy = match ecosystem {
            Ecosystem::Python => &self.config.python_policy,
            Ecosystem::NodeJs => &self.config.nodejs_policy,
            Ecosystem::Rust => &self.config.rust_policy,
            Ecosystem::System => &self.config.system_policy,
            Ecosystem::Generic => return Ok(()), // Generic has no packages
        };

        match policy.as_str() {
            "open" => Ok(()),
            "locked" => Err(AgentOSError::PermissionDenied {
                resource: format!("env.install.{ecosystem:?}"),
                operation: format!(
                    "package installation is locked for {ecosystem:?} ecosystem; \
                     requires operator approval"
                ),
            }),
            _ => {
                // Check allowlist
                if let Some(allowlist) = self.allowlists.get(&ecosystem) {
                    if allowlist.packages.iter().any(|p| p == package_name) {
                        return Ok(());
                    }
                }
                Err(AgentOSError::PermissionDenied {
                    resource: format!("env.install.{ecosystem:?}"),
                    operation: format!(
                        "package '{package_name}' is not on the curated allowlist for {ecosystem:?}; \
                         requires operator approval"
                    ),
                })
            }
        }
    }

    // ---- Action implementations ----

    async fn action_create(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'name' field".into()))?;
        validate_workspace_name(name)?;

        let ecosystem_str = params["ecosystem"].as_str().unwrap_or("generic");
        let ecosystem = Ecosystem::from_str_loose(ecosystem_str)?;

        let root_path = Self::workspace_path(&context.data_dir, &context.agent_id, name);
        let key = (context.agent_id, name.to_string());

        // First, take the write lock just long enough to reserve the slot
        // (rejecting duplicates) and release it before the slow filesystem
        // and DB work. We use a tombstone-like placeholder (`Pending`) — no,
        // simpler: hold the lock only for the duplicate check, drop, and
        // re-check on insert at the end. The (agent_id, name) key prevents
        // cross-agent collisions; in-agent races just produce the same
        // collision error on whichever call lands the insert second.
        {
            let ws = self.workspaces.read().await;
            if ws.contains_key(&key) {
                return Err(AgentOSError::KernelError {
                    reason: format!("workspace '{name}' already exists for this agent"),
                });
            }
        }

        // Slow path runs without the workspaces lock so concurrent reads /
        // installs on other workspaces aren't blocked.
        let root_clone = root_path.clone();
        let eco = ecosystem;
        let scope = agent_workspaces_root(&context.data_dir, &context.agent_id);
        let scope_clone = scope.clone();
        tokio::task::spawn_blocking(move || -> Result<(), AgentOSError> {
            // Up to `data_dir/workspaces/<agent_id>` nothing is writable from a
            // sandbox, so `create_dir_all` is safe; below it the agent can
            // plant symlinks.
            std::fs::create_dir_all(&scope_clone).map_err(|e| AgentOSError::KernelError {
                reason: format!("failed to create workspace directory: {e}"),
            })?;
            ensure_real_dir(&root_clone)?;
            let subdir = match eco {
                Ecosystem::NodeJs => Some("node_modules"),
                Ecosystem::Rust => Some("target"),
                Ecosystem::Python | Ecosystem::System | Ecosystem::Generic => None,
            };
            if let Some(sub) = subdir {
                ensure_real_dir(&root_clone.join(sub))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("workspace creation task panicked: {e}"),
        })??;

        // The venv is built by the sandbox's own `python3`, so its interpreter
        // symlink resolves inside later sandboxes (a host pyenv/conda python
        // under /home would dangle there).
        let python_venv_stub = ecosystem == Ecosystem::Python
            && match Self::create_venv(&scope, &root_path).await {
                Ok(()) => false,
                Err(reason) => {
                    tracing::warn!(
                        workspace = %name,
                        "python venv not created ({reason}); creating stub structure, env-install will fail until it can be"
                    );
                    let venv_dir = root_path.join("venv");
                    for dir in [venv_dir.clone(), venv_dir.join("bin"), venv_dir.join("lib")] {
                        ensure_real_dir(&dir).ok();
                    }
                    true
                }
            };

        let workspace = ManagedWorkspace {
            name: name.to_string(),
            agent_id: context.agent_id,
            root_path: root_path.clone(),
            ecosystem,
            created_at: chrono::Utc::now(),
            packages_installed: vec![],
        };

        // Persist before the in-memory insert so the store is the source of
        // truth on crash mid-create. If the persist fails we still surface
        // the workspace in-memory so the agent can keep working.
        if let Some(store) = &self.store {
            if let Err(e) = store.upsert(&workspace).await {
                tracing::warn!(error = %e, workspace = %name, "failed to persist workspace; in-memory state still consistent");
            }
        }

        // Final insert under write lock — re-check for the race where two
        // concurrent creates of the same (agent_id, name) both made it past
        // the read-lock duplicate check.
        {
            let mut ws = self.workspaces.write().await;
            if ws.contains_key(&key) {
                return Err(AgentOSError::KernelError {
                    reason: format!("workspace '{name}' was concurrently created by another call"),
                });
            }
            ws.insert(key, workspace);
        }

        let mut output = json!({
            "workspace": name,
            "ecosystem": ecosystem_str,
            "path": root_path.to_string_lossy(),
        });
        if python_venv_stub {
            output["warnings"] = json!([
                "python venv could not be created in the sandbox (python3/python3-venv missing, or bwrap unusable): venv is a stub and env-install will fail; see the kernel log (host-package-install can add python3-venv)"
            ]);
        }
        Ok(CapabilityResult {
            output,
            audit_metadata: json!({
                "event": "EnvironmentCreated",
                "workspace": name,
                "ecosystem": ecosystem_str,
                "path": root_path.to_string_lossy().to_string(),
                "python_venv_stub": python_venv_stub,
            }),
        })
    }

    async fn action_install(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let package = params["package"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'package' field".into()))?;
        validate_package_name(package)?;

        let workspace_name = params["workspace"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'workspace' field".into()))?;
        validate_workspace_name(workspace_name)?;

        // SECURITY: validate version string to prevent command injection.
        let version = params["version"].as_str();
        if let Some(v) = version {
            validate_version(v)?;
        }

        // Look up workspace
        let ecosystem = {
            let ws = self.workspaces.read().await;
            let workspace = ws
                .get(&(context.agent_id, workspace_name.to_string()))
                .ok_or_else(|| AgentOSError::KernelError {
                    reason: format!(
                        "workspace '{workspace_name}' not found; create it first with env.create"
                    ),
                })?;
            workspace.ecosystem
        };

        // Check package allowlist
        self.check_package_allowed(ecosystem, package)?;

        // Build the install command as (program, args) — NEVER use sh -c to avoid injection.
        let ws_path = Self::workspace_path(&context.data_dir, &context.agent_id, workspace_name);
        let (program, args): (String, Vec<String>) = match ecosystem {
            Ecosystem::Python => {
                let pip = ws_path.join("venv/bin/pip");
                let pkg_spec = match version {
                    Some(v) => format!("{package}=={v}"),
                    None => package.to_string(),
                };
                (
                    pip.to_string_lossy().to_string(),
                    vec!["install".into(), pkg_spec, "--no-cache-dir".into()],
                )
            }
            Ecosystem::NodeJs => {
                let pkg_spec = match version {
                    Some(v) => format!("{package}@{v}"),
                    None => package.to_string(),
                };
                (
                    "npm".into(),
                    vec![
                        "install".into(),
                        "--prefix".into(),
                        ws_path.to_string_lossy().to_string(),
                        pkg_spec,
                    ],
                )
            }
            Ecosystem::Rust => {
                let mut a = vec![
                    "install".into(),
                    package.to_string(),
                    "--root".into(),
                    ws_path.to_string_lossy().to_string(),
                ];
                if let Some(v) = version {
                    a.push("--version".into());
                    a.push(v.to_string());
                }
                ("cargo".into(), a)
            }
            Ecosystem::System => {
                return Err(AgentOSError::PermissionDenied {
                    resource: "env.install.system".into(),
                    operation: "system package installation requires elevated policy".into(),
                });
            }
            Ecosystem::Generic => {
                return Err(AgentOSError::SchemaValidation(
                    "cannot install packages in a generic workspace; \
                     create a Python, NodeJs, or Rust workspace instead"
                        .into(),
                ));
            }
        };

        // SECURITY: install scripts (setup.py, npm lifecycle scripts, build.rs)
        // are arbitrary code. They run sandboxed: the workspace is the only
        // writable path, kernel state and the operator's home do not exist,
        // and the environment is cleared. Network is on — installing needs it,
        // and `env.install` already requires `net.outbound:x`.
        // Argv, never `sh -c`, so a package name cannot inject.
        let ws = WorkspaceInfo {
            root: ws_path.clone(),
            ecosystem,
        };
        let sandbox = Sandbox::new("env-install")
            .bind_rw(agent_workspaces_root(&context.data_dir, &context.agent_id))
            .network(true);
        // `activate` would refuse a Python workspace whose venv cannot run
        // here; pip is invoked by path, so check it the same way.
        let sandbox = activate(sandbox, &ws)?;
        let mut cmd = sandbox.command(&ws_path, &program).await?;
        cmd.args(&args);
        let timeout = std::time::Duration::from_secs(self.config.install_timeout_secs);
        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| AgentOSError::ToolExecutionFailed {
                tool_name: "env-install".into(),
                reason: format!(
                    "package installation timed out after {}s",
                    self.config.install_timeout_secs
                ),
            })?
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "env-install".into(),
                reason: format!("failed to execute install command: {e}"),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "env-install".into(),
                reason: format!(
                    "package installation failed (exit code {}): {}",
                    output.status.code().unwrap_or(-1),
                    stderr.chars().take(500).collect::<String>()
                ),
            });
        }

        // Record the installed package
        let installed = InstalledPackage {
            name: package.to_string(),
            version: version.unwrap_or("latest").to_string(),
            ecosystem,
            installed_at: chrono::Utc::now(),
        };

        let snapshot_for_store: Option<ManagedWorkspace> = {
            let mut ws = self.workspaces.write().await;
            if let Some(workspace) = ws.get_mut(&(context.agent_id, workspace_name.to_string())) {
                workspace.packages_installed.push(installed.clone());
                Some(workspace.clone())
            } else {
                tracing::warn!(
                    workspace = workspace_name,
                    package = package,
                    "workspace was destroyed during package install; installed package is orphaned"
                );
                None
            }
        };
        // Persist OUTSIDE the lock. The destroy path holds the same write
        // lock so a destroy that lands between the snapshot clone and this
        // upsert would race; we re-verify membership under a read lock right
        // before persisting to avoid resurrecting a destroyed workspace.
        if let (Some(store), Some(snap)) = (self.store.as_ref(), snapshot_for_store) {
            let still_present = {
                let ws = self.workspaces.read().await;
                ws.contains_key(&(context.agent_id, workspace_name.to_string()))
            };
            if !still_present {
                tracing::warn!(
                    workspace = %workspace_name,
                    package = %package,
                    "workspace destroyed during package install; skipping store upsert to avoid ghost row"
                );
            } else if let Err(e) = store.upsert(&snap).await {
                tracing::warn!(error = %e, workspace = %workspace_name, package = %package, "failed to persist package install; in-memory state still consistent");
            }
        }

        let eco_str = serde_json::to_value(ecosystem)
            .unwrap_or_else(|_| Value::String(format!("{ecosystem:?}")));

        Ok(CapabilityResult {
            output: json!({
                "package": package,
                "version": installed.version,
                "ecosystem": eco_str,
                "workspace": workspace_name,
                "stdout": stdout.chars().take(1000).collect::<String>(),
            }),
            audit_metadata: json!({
                "event": "PackageInstalled",
                "package": package,
                "version": installed.version,
                "ecosystem": eco_str,
                "workspace": workspace_name,
            }),
        })
    }

    async fn action_list(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        // If no workspace is specified, list all workspaces for this agent.
        let workspace_name = params["workspace"].as_str();

        if let Some(name) = workspace_name {
            validate_workspace_name(name)?;
            return self.action_list_workspace(name, context).await;
        }

        // List all workspaces belonging to this agent.
        let ws = self.workspaces.read().await;
        let workspaces: Vec<Value> = ws
            .iter()
            .filter(|((agent_id, _), _)| *agent_id == context.agent_id)
            .map(|((_, name), workspace)| {
                let eco = serde_json::to_value(workspace.ecosystem)
                    .unwrap_or_else(|_| Value::String(format!("{:?}", workspace.ecosystem)));
                json!({
                    "name": name,
                    "ecosystem": eco,
                    "packages_count": workspace.packages_installed.len(),
                    "created_at": workspace.created_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(CapabilityResult {
            output: json!({
                "workspaces": workspaces,
                "count": workspaces.len(),
                "hint": if workspaces.is_empty() {
                    "No workspaces found. Use env-create to create one first."
                } else {
                    "Pass {\"workspace\": \"<name>\"} to list packages in a specific workspace."
                },
            }),
            audit_metadata: json!({
                "action": "list",
                "scope": "all_workspaces",
            }),
        })
    }

    async fn action_list_workspace(
        &self,
        workspace_name: &str,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let ws = self.workspaces.read().await;
        let workspace = ws
            .get(&(context.agent_id, workspace_name.to_string()))
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("workspace '{workspace_name}' not found. Use env-list without a workspace argument to see available workspaces, or env-create to create one."),
            })?;

        let packages: Vec<Value> = workspace
            .packages_installed
            .iter()
            .map(|p| {
                let eco = serde_json::to_value(p.ecosystem)
                    .unwrap_or_else(|_| Value::String(format!("{:?}", p.ecosystem)));
                json!({
                    "name": p.name,
                    "version": p.version,
                    "ecosystem": eco,
                    "installed_at": p.installed_at.to_rfc3339(),
                })
            })
            .collect();

        let eco_str = serde_json::to_value(workspace.ecosystem)
            .unwrap_or_else(|_| Value::String(format!("{:?}", workspace.ecosystem)));

        Ok(CapabilityResult {
            output: json!({
                "workspace": workspace_name,
                "ecosystem": eco_str,
                "packages": packages,
                "count": packages.len(),
            }),
            audit_metadata: json!({
                "action": "list",
                "workspace": workspace_name,
            }),
        })
    }

    async fn action_destroy(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        // Accept both "name" and "workspace" for consistency with other actions
        let workspace_name = params["workspace"]
            .as_str()
            .or_else(|| params["name"].as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("missing 'workspace' or 'name' field".into())
            })?;
        validate_workspace_name(workspace_name)?;

        let root_path = {
            let mut ws = self.workspaces.write().await;
            let workspace = ws
                .remove(&(context.agent_id, workspace_name.to_string()))
                .ok_or_else(|| AgentOSError::KernelError {
                    reason: format!("workspace '{workspace_name}' not found"),
                })?;
            workspace.root_path
        };

        if let Some(store) = &self.store {
            if let Err(e) = store
                .remove(context.agent_id, workspace_name.to_string())
                .await
            {
                tracing::warn!(error = %e, workspace = %workspace_name, "failed to delete workspace row; will be cleaned up on next boot reconciliation");
            }
        }

        // Remove directory tree
        let path_clone = root_path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), AgentOSError> {
            if path_clone.exists() {
                std::fs::remove_dir_all(&path_clone).map_err(|e| AgentOSError::KernelError {
                    reason: format!("failed to remove workspace directory: {e}"),
                })?;
            }
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::KernelError {
            reason: format!("workspace destroy task panicked: {e}"),
        })??;

        Ok(CapabilityResult {
            output: json!({
                "destroyed": workspace_name,
                "path": root_path.to_string_lossy().to_string(),
            }),
            audit_metadata: json!({
                "event": "EnvironmentDestroyed",
                "workspace": workspace_name,
                "path": root_path.to_string_lossy().to_string(),
            }),
        })
    }
}

#[async_trait]
impl WorkspaceResolver for EnvProvider {
    async fn resolve(&self, agent_id: agentos_types::AgentID, name: &str) -> Option<WorkspaceInfo> {
        let ws = self.workspaces.read().await;
        ws.get(&(agent_id, name.to_string()))
            .map(|w| WorkspaceInfo {
                root: w.root_path.clone(),
                ecosystem: w.ecosystem,
            })
    }
}

#[async_trait]
impl CapabilityProvider for EnvProvider {
    fn domain(&self) -> &str {
        "env"
    }

    fn supported_actions(&self) -> &[&str] {
        &["create", "install", "list", "destroy"]
    }

    fn required_permissions(&self, action: &str) -> Option<Vec<(String, PermissionOp)>> {
        match action {
            "create" => Some(vec![("env.create".to_string(), PermissionOp::Execute)]),
            "install" => Some(vec![
                ("env.install".to_string(), PermissionOp::Execute),
                ("net.outbound".to_string(), PermissionOp::Execute),
            ]),
            "list" => Some(vec![("env.list".to_string(), PermissionOp::Read)]),
            "destroy" => Some(vec![("env.destroy".to_string(), PermissionOp::Execute)]),
            _ => None,
        }
    }

    async fn execute(
        &self,
        action: &str,
        params: Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        match action {
            "create" => self.action_create(&params, context).await,
            "install" => self.action_install(&params, context).await,
            "list" => self.action_list(&params, context).await,
            "destroy" => self.action_destroy(&params, context).await,
            other => Err(AgentOSError::KernelError {
                reason: format!("unknown env action '{other}'"),
            }),
        }
    }

    fn description(&self) -> &str {
        "Manage isolated workspaces with package installation (Python, Node.js, Rust)"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, TaskID, TraceID};
    use tempfile::TempDir;

    fn make_provider() -> EnvProvider {
        let mut allowlists = HashMap::new();
        allowlists.insert(
            Ecosystem::Python,
            PackageAllowlist {
                packages: vec![
                    "flask".into(),
                    "pytest".into(),
                    "requests".into(),
                    "numpy".into(),
                ],
            },
        );
        allowlists.insert(
            Ecosystem::NodeJs,
            PackageAllowlist {
                packages: vec!["express".into(), "jest".into()],
            },
        );
        EnvProvider::new(EnvConfig::default(), allowlists)
    }

    fn make_context(data_dir: &Path) -> CapabilityContext {
        CapabilityContext {
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
            data_dir: data_dir.to_path_buf(),
            permissions: agentos_types::PermissionSet::default(),
            workspace_paths: vec![],
            agent_home: data_dir.join("agents").join("test"),
            workspace_paths_executable: vec![],
        }
    }

    #[test]
    fn provider_metadata() {
        let p = make_provider();
        assert_eq!(p.domain(), "env");
        assert_eq!(
            p.supported_actions(),
            &["create", "install", "list", "destroy"]
        );
        assert!(p.required_permissions("create").is_some());
        assert!(p.required_permissions("install").is_some());
        assert!(p.required_permissions("list").is_some());
        assert!(p.required_permissions("destroy").is_some());
        assert!(p.required_permissions("nonexistent").is_none());
    }

    #[test]
    fn install_permissions_include_network() {
        let p = make_provider();
        let perms = p.required_permissions("install").unwrap();
        assert!(perms.iter().any(|(r, _)| r == "net.outbound"));
    }

    #[test]
    fn validate_workspace_name_good() {
        assert!(validate_workspace_name("my-project").is_ok());
        assert!(validate_workspace_name("test_env_1").is_ok());
        assert!(validate_workspace_name("a").is_ok());
    }

    #[test]
    fn validate_workspace_name_bad() {
        assert!(validate_workspace_name("").is_err());
        assert!(validate_workspace_name("a/b").is_err());
        assert!(validate_workspace_name("../escape").is_err());
        assert!(validate_workspace_name("a b").is_err());
        assert!(validate_workspace_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn validate_package_name_good() {
        assert!(validate_package_name("flask").is_ok());
        assert!(validate_package_name("scikit-learn").is_ok());
        assert!(validate_package_name("python-dotenv").is_ok());
        assert!(validate_package_name("beautifulsoup4").is_ok());
        assert!(validate_package_name("pyyaml").is_ok());
    }

    #[test]
    fn validate_package_name_bad() {
        assert!(validate_package_name("").is_err());
        assert!(validate_package_name("flask; rm -rf /").is_err());
        assert!(validate_package_name("pkg && malicious").is_err());
        assert!(validate_package_name(&"x".repeat(129)).is_err());
    }

    #[test]
    fn check_package_allowed_curated() {
        let p = make_provider();
        assert!(p.check_package_allowed(Ecosystem::Python, "flask").is_ok());
        assert!(p
            .check_package_allowed(Ecosystem::Python, "unknown-pkg")
            .is_err());
    }

    #[test]
    fn check_package_allowed_locked() {
        let p = make_provider();
        assert!(p.check_package_allowed(Ecosystem::System, "curl").is_err());
    }

    #[test]
    fn check_package_allowed_open() {
        let config = EnvConfig {
            python_policy: "open".into(),
            ..Default::default()
        };
        let p = EnvProvider::new(config, HashMap::new());
        assert!(p
            .check_package_allowed(Ecosystem::Python, "anything")
            .is_ok());
    }

    #[test]
    fn from_config_populates_allowlists() {
        let settings = crate::config::EnvSettings {
            python_allowlist: vec!["flask".into(), "requests".into()],
            nodejs_allowlist: vec!["express".into()],
            ..Default::default()
        };
        let p = EnvProvider::from_config(&settings);
        assert!(p.check_package_allowed(Ecosystem::Python, "flask").is_ok());
        assert!(p
            .check_package_allowed(Ecosystem::Python, "requests")
            .is_ok());
        assert!(p
            .check_package_allowed(Ecosystem::Python, "evil-pkg")
            .is_err());
        assert!(p
            .check_package_allowed(Ecosystem::NodeJs, "express")
            .is_ok());
        // Rust allowlist empty → curated policy denies all
        assert!(p.check_package_allowed(Ecosystem::Rust, "serde").is_err());
    }

    #[test]
    fn from_config_empty_keeps_curated_locked() {
        let settings = crate::config::EnvSettings::default();
        let p = EnvProvider::from_config(&settings);
        assert!(p.check_package_allowed(Ecosystem::Python, "flask").is_err());
        assert!(p.check_package_allowed(Ecosystem::System, "curl").is_err());
    }

    #[tokio::test]
    async fn workspace_state_survives_provider_restart() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("workspaces.db");
        let settings = crate::config::EnvSettings::default();

        let agent_id = AgentID::new();
        let ctx = CapabilityContext {
            agent_id,
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
            data_dir: tmp.path().to_path_buf(),
            permissions: agentos_types::PermissionSet::default(),
            workspace_paths: vec![],
            agent_home: tmp.path().join("agents").join("test"),
            workspace_paths_executable: vec![],
        };

        // First provider — create a generic workspace via the action.
        {
            let store = Arc::new(
                crate::workspace_store::WorkspaceStore::open(db_path.clone())
                    .await
                    .unwrap(),
            );
            let p = EnvProvider::from_config_with_store(&settings, store)
                .await
                .unwrap();
            p.execute(
                "create",
                json!({"name":"survivor","ecosystem":"generic"}),
                &ctx,
            )
            .await
            .unwrap();
        }

        // Second provider — fresh in-memory map, must load the row from disk.
        let store2 = Arc::new(
            crate::workspace_store::WorkspaceStore::open(db_path)
                .await
                .unwrap(),
        );
        let p2 = EnvProvider::from_config_with_store(&settings, store2)
            .await
            .unwrap();
        let result = p2.execute("list", json!({}), &ctx).await.unwrap();
        let names: Vec<String> = result.output["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "survivor"),
            "workspace should survive provider restart; got {names:?}"
        );
    }

    #[test]
    fn ecosystem_from_str() {
        assert_eq!(
            Ecosystem::from_str_loose("python").unwrap(),
            Ecosystem::Python
        );
        assert_eq!(Ecosystem::from_str_loose("py").unwrap(), Ecosystem::Python);
        assert_eq!(
            Ecosystem::from_str_loose("nodejs").unwrap(),
            Ecosystem::NodeJs
        );
        assert_eq!(
            Ecosystem::from_str_loose("node").unwrap(),
            Ecosystem::NodeJs
        );
        assert_eq!(Ecosystem::from_str_loose("npm").unwrap(), Ecosystem::NodeJs);
        assert_eq!(Ecosystem::from_str_loose("rust").unwrap(), Ecosystem::Rust);
        assert_eq!(Ecosystem::from_str_loose("cargo").unwrap(), Ecosystem::Rust);
        assert_eq!(
            Ecosystem::from_str_loose("generic").unwrap(),
            Ecosystem::Generic
        );
        assert!(Ecosystem::from_str_loose("unknown").is_err());
    }

    #[tokio::test]
    async fn create_generic_workspace() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        let result = provider
            .execute(
                "create",
                json!({"name": "test-ws", "ecosystem": "generic"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(result.output["workspace"], "test-ws");
        let ws_dir = tmp
            .path()
            .join("workspaces")
            .join(ctx.agent_id.to_string())
            .join("test-ws");
        assert!(ws_dir.exists());
    }

    #[tokio::test]
    async fn create_python_workspace() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        let result = provider
            .execute(
                "create",
                json!({"name": "py-env", "ecosystem": "python"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(result.output["ecosystem"], "python");
        let ws_dir = tmp
            .path()
            .join("workspaces")
            .join(ctx.agent_id.to_string())
            .join("py-env");
        assert!(ws_dir.join("venv").exists());
    }

    #[tokio::test]
    async fn create_nodejs_workspace() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        provider
            .execute(
                "create",
                json!({"name": "node-env", "ecosystem": "nodejs"}),
                &ctx,
            )
            .await
            .unwrap();

        let ws_dir = tmp
            .path()
            .join("workspaces")
            .join(ctx.agent_id.to_string())
            .join("node-env");
        assert!(ws_dir.join("node_modules").exists());
    }

    #[tokio::test]
    async fn create_duplicate_workspace_fails() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        provider
            .execute("create", json!({"name": "dup-ws"}), &ctx)
            .await
            .unwrap();

        let err = provider
            .execute("create", json!({"name": "dup-ws"}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("already exists"));
    }

    #[tokio::test]
    async fn list_empty_workspace() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        provider
            .execute("create", json!({"name": "list-ws"}), &ctx)
            .await
            .unwrap();

        let result = provider
            .execute("list", json!({"workspace": "list-ws"}), &ctx)
            .await
            .unwrap();

        assert_eq!(result.output["count"], 0);
        assert_eq!(result.output["packages"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn destroy_workspace() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        provider
            .execute("create", json!({"name": "destroy-ws"}), &ctx)
            .await
            .unwrap();
        let ws_dir = tmp
            .path()
            .join("workspaces")
            .join(ctx.agent_id.to_string())
            .join("destroy-ws");
        assert!(ws_dir.exists());

        provider
            .execute("destroy", json!({"name": "destroy-ws"}), &ctx)
            .await
            .unwrap();
        assert!(!ws_dir.exists());
    }

    #[tokio::test]
    async fn destroy_nonexistent_workspace_fails() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        let err = provider
            .execute("destroy", json!({"name": "nope"}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not found"));
    }

    #[tokio::test]
    async fn unknown_action_fails() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        let err = provider
            .execute("snapshot", json!({}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("unknown env action"));
    }

    #[tokio::test]
    async fn agent_isolation() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();

        let ctx_a = make_context(tmp.path());
        let ctx_b = CapabilityContext {
            agent_id: AgentID::new(), // Different agent
            ..make_context(tmp.path())
        };

        // Agent A creates workspace
        provider
            .execute("create", json!({"name": "shared-name"}), &ctx_a)
            .await
            .unwrap();

        // Agent B cannot list Agent A's workspace
        let err = provider
            .execute("list", json!({"workspace": "shared-name"}), &ctx_b)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not found"));
    }

    #[test]
    fn workspace_path_includes_agent_id() {
        let agent_id = AgentID::new();
        let path = EnvProvider::workspace_path(Path::new("/data"), &agent_id, "my-ws");
        assert_eq!(
            path,
            PathBuf::from(format!("/data/workspaces/{}/my-ws", agent_id))
        );
    }

    #[test]
    fn validate_version_good() {
        assert!(validate_version("3.0.0").is_ok());
        assert!(validate_version(">=2.0").is_ok());
        assert!(validate_version("~=1.4").is_ok());
        assert!(validate_version("^1.0.0").is_ok());
        assert!(validate_version("*").is_ok());
    }

    #[test]
    fn validate_version_injection() {
        assert!(validate_version("3.0.0; rm -rf /").is_err());
        assert!(validate_version("1.0 && curl evil.com").is_err());
        assert!(validate_version("$(malicious)").is_err());
        assert!(validate_version("1.0`whoami`").is_err());
    }

    #[tokio::test]
    async fn install_nonexistent_workspace_fails() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        let err = provider
            .execute(
                "install",
                json!({"package": "flask", "workspace": "nope"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not found"));
    }

    #[tokio::test]
    async fn install_disallowed_package_fails() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        provider
            .execute(
                "create",
                json!({"name": "py-ws", "ecosystem": "python"}),
                &ctx,
            )
            .await
            .unwrap();

        let err = provider
            .execute(
                "install",
                json!({"package": "unknown-evil-pkg", "workspace": "py-ws"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not on the curated allowlist"));
    }

    #[tokio::test]
    async fn install_into_generic_workspace_fails() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider();
        let ctx = make_context(tmp.path());

        provider
            .execute(
                "create",
                json!({"name": "gen-ws", "ecosystem": "generic"}),
                &ctx,
            )
            .await
            .unwrap();

        let err = provider
            .execute(
                "install",
                json!({"package": "flask", "workspace": "gen-ws"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("cannot install packages in a generic workspace"));
    }

    #[tokio::test]
    async fn install_into_system_workspace_fails() {
        let tmp = TempDir::new().unwrap();
        let config = EnvConfig {
            system_policy: "open".into(), // Even with open policy, system is blocked
            ..Default::default()
        };
        let provider = EnvProvider::new(config, HashMap::new());
        let ctx = make_context(tmp.path());

        provider
            .execute(
                "create",
                json!({"name": "sys-ws", "ecosystem": "system"}),
                &ctx,
            )
            .await
            .unwrap();

        let err = provider
            .execute(
                "install",
                json!({"package": "curl", "workspace": "sys-ws"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("elevated policy"));
    }

    /// A sandboxed command can replace a workspace dir with a symlink. bwrap
    /// resolves bind sources on the host, so binding that path would mount
    /// the link target — here the kernel data dir — into the next sandbox.
    #[tokio::test]
    async fn swapped_workspace_symlink_does_not_expose_host_paths() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("vault.db"), "SECRET").unwrap();
        let provider = Arc::new(make_provider());
        let ctx = make_context(tmp.path());
        std::fs::create_dir_all(&ctx.agent_home).unwrap();
        let scope = agent_workspaces_root(tmp.path(), &ctx.agent_id);

        // env-create never creates through a planted link.
        let elsewhere = TempDir::new().unwrap();
        std::fs::create_dir_all(&scope).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), scope.join("planted")).unwrap();
        let err = provider
            .execute(
                "create",
                json!({"name": "planted", "ecosystem": "nodejs"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not a plain directory"), "{err}");
        assert!(!elsewhere.path().join("node_modules").exists());

        provider
            .execute("create", json!({"name": "w"}), &ctx)
            .await
            .unwrap();
        if !agentos_tools::sandbox_fs::bwrap_usable().await {
            eprintln!("skipping sandbox half: bwrap cannot sandbox on this host");
            return;
        }
        // What a proc-spawn'd `sh` in the agent's scope can do.
        std::fs::remove_dir(scope.join("w")).unwrap();
        std::os::unix::fs::symlink("../..", scope.join("w")).unwrap();
        assert!(scope.join("w").join("vault.db").exists(), "fixture");

        let build = crate::managed_build::BuildProvider::with_resolver(
            crate::managed_build::BuildConfig {
                allowed_commands: vec![],
                ..Default::default()
            },
            provider.clone(),
        );
        // Positive control: the same command does read files in scope.
        std::fs::write(ctx.agent_home.join("probe"), "VISIBLE").unwrap();
        for (path, visible) in [
            (ctx.agent_home.join("probe"), true),
            (tmp.path().join("vault.db"), false),
            (scope.join("w").join("vault.db"), false),
        ] {
            let out = build
                .execute(
                    "run",
                    json!({"workspace": "w", "command": format!("python3 -c print(open('{}').read())", path.display()),
                           "working_dir": ctx.agent_home.display().to_string()}),
                    &ctx,
                )
                .await
                .unwrap();
            let text = out.output["output"].as_str().unwrap();
            let expected = if visible { "VISIBLE" } else { "SECRET" };
            assert_eq!(
                text.contains(expected),
                visible,
                "{}: {text}",
                path.display()
            );
        }
    }

    /// Real installs through the sandbox, then use from `build-run`. Needs
    /// network, bwrap, python3-venv and npm: `cargo test -p agentos-kernel
    /// --lib sandboxed_install -- --ignored`.
    #[tokio::test]
    #[ignore = "network: installs real packages"]
    async fn sandboxed_install_is_usable_from_build_run() {
        let tmp = TempDir::new().unwrap();
        let config = EnvConfig {
            python_policy: "open".into(),
            nodejs_policy: "open".into(),
            ..EnvConfig::default()
        };
        let provider = Arc::new(EnvProvider::new(config, HashMap::new()));
        let mut ctx = make_context(tmp.path());
        std::fs::create_dir_all(&ctx.agent_home).unwrap();
        ctx.permissions
            .grant_op("net.outbound".into(), PermissionOp::Execute, None);

        for (ws, eco, pkg) in [("py", "python", "six"), ("js", "nodejs", "is-number")] {
            let created = provider
                .execute("create", json!({"name": ws, "ecosystem": eco}), &ctx)
                .await
                .unwrap();
            assert!(created.output.get("warnings").is_none(), "{created:?}");
            provider
                .execute("install", json!({"workspace": ws, "package": pkg}), &ctx)
                .await
                .unwrap();
        }
        // The venv interpreter must resolve inside later sandboxes.
        let python = std::fs::canonicalize(
            agent_workspaces_root(tmp.path(), &ctx.agent_id).join("py/venv/bin/python"),
        )
        .unwrap();
        assert!(!python.starts_with("/home"), "{}", python.display());

        let build = crate::managed_build::BuildProvider::with_resolver(
            Default::default(),
            provider.clone(),
        );
        for (ws, cmd) in [
            ("py", "python -c __import__(\"six\")"),
            ("js", "node -e require(\"is-number\")"),
        ] {
            let out = build
                .execute("run", json!({"workspace": ws, "command": cmd}), &ctx)
                .await
                .unwrap();
            assert_eq!(out.output["exit_code"], 0, "{ws}: {}", out.output["output"]);
        }
    }
}
