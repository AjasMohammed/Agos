//! `skill-create` — agent-facing tool for authoring and installing skills at
//! runtime.
//!
//! The kernel exposes `SkillInstall`/`SkillRemove` as kernel-only commands, so
//! agents have no direct path to add a skill. This tool closes that gap: it
//! writes a `SKILL.toml` + prompt file under the configured `user_skills_dir`
//! and asks the kernel to load the result into the live registry.
//!
//! Safety:
//! * Trust tier is forced to `community` — agent-authored skills never claim
//!   `core` provenance.
//! * The skill name is restricted to lowercase kebab-case (`a-z0-9-`), with
//!   leading/trailing dashes rejected, so it cannot escape `user_skills_dir`.
//! * The manifest's `risk_class` is `control_plane`, so every invocation is
//!   gated by the approval hook regardless of approval mode.

use crate::agent_manual::SharedInstalledSkills;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Abstraction over "load a skill directory into the running kernel".
///
/// The kernel implements this with access to its `SkillRegistry` and the
/// `installed_skills_snapshot`. Defining it here keeps `agentos-tools`
/// independent of `agentos-skills` and `agentos-kernel`.
#[async_trait]
pub trait SkillInstaller: Send + Sync {
    /// Load the skill at `dir` and install it into the live registry, then
    /// refresh the shared `installed_skills` snapshot.
    ///
    /// The duplicate decision is authoritative here, made against the registry
    /// under its write lock — when `overwrite` is false and a skill with the
    /// same name is already installed, this must fail (fail-closed). The
    /// tool-side snapshot check is only a cheap early-out.
    ///
    /// On success returns `(name, version)`.
    async fn install_from_dir(
        &self,
        dir: &Path,
        overwrite: bool,
    ) -> Result<(String, String), String>;
}

pub type SharedSkillInstaller = Arc<dyn SkillInstaller>;

/// `skill-create` tool.
pub struct SkillCreateTool {
    /// Root directory where agent-authored skills live (e.g. `skills/user`).
    user_skills_dir: PathBuf,
    /// Hook used to install the freshly-written skill into the kernel.
    installer: SharedSkillInstaller,
    /// Live snapshot — read for duplicate detection so two concurrent calls
    /// don't both think the slot is empty.
    installed_skills: SharedInstalledSkills,
}

impl SkillCreateTool {
    pub fn new(
        user_skills_dir: PathBuf,
        installer: SharedSkillInstaller,
        installed_skills: SharedInstalledSkills,
    ) -> Self {
        Self {
            user_skills_dir,
            installer,
            installed_skills,
        }
    }

    /// Strict skill-name validation. Lowercase ASCII alphanumeric and dashes,
    /// 2..64 chars, must start and end with `[a-z0-9]`. Prevents traversal
    /// (`..`, `/`, `\`), absolute paths, and shell-glob characters.
    fn validate_name(name: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("skill name is empty".into());
        }
        // Character allowlist first, so a multibyte name yields the precise
        // "invalid character" error rather than a misleading byte-length one.
        let bytes = name.as_bytes();
        for &c in bytes {
            let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-';
            if !ok {
                return Err(format!(
                    "skill name '{name}' contains invalid character (allowed: a-z0-9-)"
                ));
            }
        }
        // Length bound: now all chars are single-byte ASCII, so len == char count.
        if name.len() < 2 || name.len() > 64 {
            return Err(format!(
                "skill name '{name}' must be 2..64 chars (got {len})",
                len = name.len()
            ));
        }
        if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
            return Err(format!("skill name '{name}' may not start or end with '-'"));
        }
        Ok(())
    }
}

#[async_trait]
impl AgentTool for SkillCreateTool {
    fn name(&self) -> &str {
        "skill-create"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Approval is enforced by `risk_class = control_plane` in the
        // manifest — every call gets human review. No additional permission
        // is required because the write lands in the kernel-controlled
        // `user_skills_dir`, not the agent's workspace.
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "skill-create requires 'name' (kebab-case identifier, e.g. 'alert-builder')"
                        .into(),
                )
            })?
            .trim()
            .to_string();

        Self::validate_name(&name).map_err(AgentOSError::SchemaValidation)?;

        let description = payload
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "skill-create requires 'description' (one-sentence summary)".into(),
                )
            })?
            .trim()
            .to_string();
        if description.is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "skill-create 'description' may not be empty".into(),
            ));
        }

        let system_prompt = payload
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "skill-create requires 'system_prompt' (the skill's instructions/recipe)"
                        .into(),
                )
            })?
            .to_string();
        if system_prompt.trim().is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "skill-create 'system_prompt' may not be empty".into(),
            ));
        }

        let version = payload
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("0.1.0")
            .trim()
            .to_string();

        let author = payload
            .get("author")
            .and_then(|v| v.as_str())
            .unwrap_or("agent-authored")
            .trim()
            .to_string();

        let required_tools: Vec<String> = payload
            .get("required_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let optional_tools: Vec<String> = payload
            .get("optional_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let max_cost_per_run = payload
            .get("max_cost_per_run")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.10);
        let max_tokens_per_run = payload
            .get("max_tokens_per_run")
            .and_then(|v| v.as_u64())
            .unwrap_or(8_000);

        let overwrite = payload
            .get("overwrite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Cheap early-out: reject obvious duplicates before any filesystem
        // write. The snapshot is a lagging copy of the registry, so this is
        // advisory only — the authoritative duplicate decision is made by the
        // installer under the registry write lock (closes the TOCTOU window).
        if !overwrite {
            let snapshot = self.installed_skills.read().await;
            if snapshot.iter().any(|s| s.name.eq_ignore_ascii_case(&name)) {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "skill-create".into(),
                    reason: format!(
                        "skill '{name}' already installed. Pass overwrite=true to replace it."
                    ),
                });
            }
        }

        // Compose the TOML manifest. Trust tier is forced — never honour an
        // agent-supplied `trust_tier`.
        let manifest_toml = render_manifest_toml(
            &name,
            &version,
            &description,
            &author,
            &required_tools,
            &optional_tools,
            max_cost_per_run,
            max_tokens_per_run,
        );

        // Write to <user_skills_dir>/<name>/{SKILL.toml,prompt.md}. Use
        // spawn_blocking — this is sync FS I/O on the async runtime.
        let dir = self.user_skills_dir.join(&name);
        let manifest_path = dir.join("SKILL.toml");
        let prompt_path = dir.join("prompt.md");
        let prompt_body = system_prompt.clone();
        // Track whether the directory existed before this call so failure
        // cleanup never deletes an existing skill's files (overwrite path).
        let dir_pre_existed = dir.exists();
        let dir_for_io = dir.clone();
        let root_for_io = self.user_skills_dir.clone();
        let write_result: Result<(), std::io::Error> = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir_for_io)?;
            // Defense-in-depth containment backstop: `name` is already
            // restricted to `[a-z0-9-]` (so it cannot contain `..`, `/`, or
            // `\`), but verify the *resolved* path still lives under the
            // user-skills root. This catches a symlinked/misconfigured root or
            // any future weakening of `validate_name` before we write.
            let root_canon = std::fs::canonicalize(&root_for_io)?;
            let dir_canon = std::fs::canonicalize(&dir_for_io)?;
            if !dir_canon.starts_with(&root_canon) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "resolved skill path escapes user_skills_dir",
                ));
            }
            std::fs::write(dir_for_io.join("SKILL.toml"), manifest_toml)?;
            std::fs::write(dir_for_io.join("prompt.md"), prompt_body)?;
            Ok(())
        })
        .await
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "skill-create".into(),
            reason: format!("skill-create write task panicked: {e}"),
        })?;
        write_result.map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "skill-create".into(),
            reason: format!("failed to write skill files under {}: {e}", dir.display()),
        })?;

        // Hand off to the kernel-side installer (loads manifest, registers,
        // refreshes the agent-manual snapshot). The installer makes the
        // authoritative duplicate decision under the registry write lock.
        let (installed_name, installed_version) =
            match self.installer.install_from_dir(&dir, overwrite).await {
                Ok(v) => v,
                Err(e) => {
                    // Don't leave a half-written skill directory behind on
                    // rejection — it would litter the kernel-controlled skills
                    // dir and could be retried on next boot. Only clean up a
                    // directory THIS call created; never delete files that
                    // pre-existed (e.g. the overwrite-of-an-invalid-update or
                    // TOCTOU-duplicate paths, where the existing skill must
                    // keep its files).
                    if !dir_pre_existed {
                        let dir_for_cleanup = dir.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            std::fs::remove_dir_all(&dir_for_cleanup)
                        })
                        .await;
                    }
                    return Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "skill-create".into(),
                        reason: format!("kernel rejected skill '{name}': {e}"),
                    });
                }
            };

        Ok(json!({
            "name": installed_name,
            "version": installed_version,
            "manifest_path": manifest_path.display().to_string(),
            "prompt_path": prompt_path.display().to_string(),
            "trust_tier": "community",
            "status": "installed",
            "note": "Skill is live. Inspect with agent-manual {\"section\": \"skills\", \"skill\": \"<name>\"} or fetch the recipe with skill-prompt."
        }))
    }
}

/// Render the SKILL.toml for an agent-authored skill. Centralised so the
/// schema (and trust-tier override) is in one place.
#[allow(clippy::too_many_arguments)]
fn render_manifest_toml(
    name: &str,
    version: &str,
    description: &str,
    author: &str,
    required_tools: &[String],
    optional_tools: &[String],
    max_cost_per_run: f64,
    max_tokens_per_run: u64,
) -> String {
    fn toml_str(s: &str) -> String {
        // toml::Value::String::to_string already produces a properly-quoted
        // TOML string literal (handles escaping). Cheap dependency reuse.
        toml::Value::String(s.to_string()).to_string()
    }
    fn toml_str_array(items: &[String]) -> String {
        let parts: Vec<String> = items.iter().map(|s| toml_str(s)).collect();
        format!("[{}]", parts.join(", "))
    }

    format!(
        "[skill]\n\
         name = {name}\n\
         version = {version}\n\
         description = {description}\n\
         author = {author}\n\
         trust_tier = \"community\"\n\
         \n\
         [agent]\n\
         system_prompt_file = \"prompt.md\"\n\
         \n\
         [tools]\n\
         required = {required}\n\
         optional = {optional}\n\
         \n\
         [budget]\n\
         max_cost_per_run = {max_cost}\n\
         max_tokens_per_run = {max_tokens}\n",
        name = toml_str(name),
        version = toml_str(version),
        description = toml_str(description),
        author = toml_str(author),
        required = toml_str_array(required_tools),
        optional = toml_str_array(optional_tools),
        max_cost = max_cost_per_run,
        max_tokens = max_tokens_per_run,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_manual::{SharedInstalledSkills, SkillSummary};
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    struct NoopInstaller {
        installed: tokio::sync::Mutex<Vec<PathBuf>>,
    }
    #[async_trait]
    impl SkillInstaller for NoopInstaller {
        async fn install_from_dir(
            &self,
            dir: &Path,
            _overwrite: bool,
        ) -> Result<(String, String), String> {
            self.installed.lock().await.push(dir.to_path_buf());
            // Parse the manifest the tool just wrote so the returned
            // name/version reflect what's on disk — keeps the test honest.
            let body = std::fs::read_to_string(dir.join("SKILL.toml"))
                .map_err(|e| format!("read SKILL.toml failed: {e}"))?;
            let value: toml::Value =
                toml::from_str(&body).map_err(|e| format!("parse failed: {e}"))?;
            let name = value
                .get("skill")
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str())
                .ok_or("no skill.name")?
                .to_string();
            let version = value
                .get("skill")
                .and_then(|s| s.get("version"))
                .and_then(|v| v.as_str())
                .ok_or("no skill.version")?
                .to_string();
            Ok((name, version))
        }
    }

    struct FailingInstaller;
    #[async_trait]
    impl SkillInstaller for FailingInstaller {
        async fn install_from_dir(
            &self,
            _dir: &Path,
            _overwrite: bool,
        ) -> Result<(String, String), String> {
            Err("manifest invalid".into())
        }
    }

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

    fn empty_snapshot() -> SharedInstalledSkills {
        Arc::new(RwLock::new(Vec::new()))
    }

    fn snapshot_with(name: &str) -> SharedInstalledSkills {
        Arc::new(RwLock::new(vec![SkillSummary {
            name: name.into(),
            version: "0.1.0".into(),
            description: "existing".into(),
            author: "test".into(),
            trust_tier: "community".into(),
            roles: vec![],
            schedule: None,
            events: vec![],
            tools_required: vec![],
            tools_optional: vec![],
            permissions_required: vec![],
            max_cost_per_run: 0.10,
            max_tokens_per_run: 8000,
            system_prompt: "existing".into(),
        }]))
    }

    #[test]
    fn validate_name_rejects_traversal() {
        for bad in [
            "..",
            "../escape",
            "/abs",
            "name/with/slash",
            "name\\back",
            "Caps",
            "with space",
            "trailing-",
            "-leading",
            "a", // too short
        ] {
            assert!(
                SkillCreateTool::validate_name(bad).is_err(),
                "expected '{bad}' rejected"
            );
        }
    }

    #[test]
    fn validate_name_accepts_kebab() {
        for ok in ["alert-builder", "cost-optimizer", "skill-1", "a1"] {
            assert!(
                SkillCreateTool::validate_name(ok).is_ok(),
                "expected '{ok}' accepted"
            );
        }
    }

    #[tokio::test]
    async fn happy_path_writes_files_and_installs() {
        let tmp = TempDir::new().unwrap();
        let installer = Arc::new(NoopInstaller {
            installed: tokio::sync::Mutex::new(Vec::new()),
        });
        let tool = SkillCreateTool::new(
            tmp.path().to_path_buf(),
            installer.clone(),
            empty_snapshot(),
        );
        let result = tool
            .execute(
                json!({
                    "name": "test-skill",
                    "description": "A test skill",
                    "system_prompt": "You are the test skill. Do the thing.",
                    "required_tools": ["notify-user"],
                    "max_cost_per_run": 0.05,
                    "max_tokens_per_run": 4000,
                }),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(result["name"], "test-skill");
        assert_eq!(result["trust_tier"], "community");
        assert_eq!(result["status"], "installed");

        let manifest_body =
            std::fs::read_to_string(tmp.path().join("test-skill/SKILL.toml")).unwrap();
        assert!(manifest_body.contains("name = \"test-skill\""));
        assert!(manifest_body.contains("trust_tier = \"community\""));
        assert!(manifest_body.contains("required = [\"notify-user\"]"));

        let prompt_body = std::fs::read_to_string(tmp.path().join("test-skill/prompt.md")).unwrap();
        assert_eq!(prompt_body, "You are the test skill. Do the thing.");

        // Verify installer was called with the right path.
        let calls = installer.installed.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], tmp.path().join("test-skill"));
    }

    #[tokio::test]
    async fn forces_community_trust_tier_even_when_caller_supplies_core() {
        let tmp = TempDir::new().unwrap();
        let installer = Arc::new(NoopInstaller {
            installed: tokio::sync::Mutex::new(Vec::new()),
        });
        let tool = SkillCreateTool::new(tmp.path().to_path_buf(), installer, empty_snapshot());
        // Caller tries to claim core trust; tool ignores the field and
        // always writes community.
        let _ = tool
            .execute(
                json!({
                    "name": "sneaky",
                    "description": "x",
                    "system_prompt": "y",
                    "trust_tier": "core",
                }),
                ctx(),
            )
            .await
            .unwrap();
        let body = std::fs::read_to_string(tmp.path().join("sneaky/SKILL.toml")).unwrap();
        assert!(body.contains("trust_tier = \"community\""));
        assert!(!body.contains("trust_tier = \"core\""));
    }

    #[tokio::test]
    async fn rejects_traversal_in_name() {
        let tmp = TempDir::new().unwrap();
        let installer = Arc::new(NoopInstaller {
            installed: tokio::sync::Mutex::new(Vec::new()),
        });
        let tool = SkillCreateTool::new(tmp.path().to_path_buf(), installer, empty_snapshot());
        let err = tool
            .execute(
                json!({"name": "../escape", "description": "x", "system_prompt": "y"}),
                ctx(),
            )
            .await
            .unwrap_err();
        match err {
            AgentOSError::SchemaValidation(msg) => assert!(msg.contains("invalid character")),
            other => panic!("expected SchemaValidation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_duplicate_without_overwrite() {
        let tmp = TempDir::new().unwrap();
        let installer = Arc::new(NoopInstaller {
            installed: tokio::sync::Mutex::new(Vec::new()),
        });
        let tool = SkillCreateTool::new(tmp.path().to_path_buf(), installer, snapshot_with("dupe"));
        let err = tool
            .execute(
                json!({"name": "dupe", "description": "x", "system_prompt": "y"}),
                ctx(),
            )
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("already installed"));
        assert!(msg.contains("overwrite=true"));
    }

    #[tokio::test]
    async fn allows_duplicate_when_overwrite_true() {
        let tmp = TempDir::new().unwrap();
        let installer = Arc::new(NoopInstaller {
            installed: tokio::sync::Mutex::new(Vec::new()),
        });
        let tool = SkillCreateTool::new(tmp.path().to_path_buf(), installer, snapshot_with("dupe"));
        let result = tool
            .execute(
                json!({
                    "name": "dupe",
                    "description": "x",
                    "system_prompt": "y",
                    "overwrite": true,
                }),
                ctx(),
            )
            .await
            .unwrap();
        assert_eq!(result["status"], "installed");
    }

    #[tokio::test]
    async fn missing_required_fields_surface_schema_errors() {
        let tmp = TempDir::new().unwrap();
        let installer = Arc::new(NoopInstaller {
            installed: tokio::sync::Mutex::new(Vec::new()),
        });
        let tool = SkillCreateTool::new(tmp.path().to_path_buf(), installer, empty_snapshot());
        let err = tool.execute(json!({}), ctx()).await.unwrap_err();
        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    }

    #[tokio::test]
    async fn installer_failure_surfaces_as_execution_failure() {
        let tmp = TempDir::new().unwrap();
        let tool = SkillCreateTool::new(
            tmp.path().to_path_buf(),
            Arc::new(FailingInstaller),
            empty_snapshot(),
        );
        let err = tool
            .execute(
                json!({"name": "fail-test", "description": "x", "system_prompt": "y"}),
                ctx(),
            )
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("manifest invalid"));
        // Cleanup: a freshly-created directory must be removed on rejection so
        // a rejected skill doesn't litter the skills dir / get retried at boot.
        assert!(
            !tmp.path().join("fail-test").exists(),
            "rejected fresh skill dir should be cleaned up"
        );
    }

    #[tokio::test]
    async fn installer_failure_preserves_preexisting_dir() {
        let tmp = TempDir::new().unwrap();
        // Simulate a pre-existing skill directory (e.g. overwrite path).
        let existing = tmp.path().join("keep-me");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("marker.txt"), "original").unwrap();

        let tool = SkillCreateTool::new(
            tmp.path().to_path_buf(),
            Arc::new(FailingInstaller),
            // Snapshot reports it installed so overwrite is required to proceed
            // past the pre-check.
            snapshot_with("keep-me"),
        );
        let err = tool
            .execute(
                json!({
                    "name": "keep-me",
                    "description": "x",
                    "system_prompt": "y",
                    "overwrite": true,
                }),
                ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("manifest invalid"));
        // The directory pre-existed, so cleanup must NOT delete it.
        assert!(
            existing.exists(),
            "pre-existing skill dir must survive a rejected overwrite"
        );
    }

    #[tokio::test]
    async fn description_with_quotes_is_escaped() {
        let tmp = TempDir::new().unwrap();
        let installer = Arc::new(NoopInstaller {
            installed: tokio::sync::Mutex::new(Vec::new()),
        });
        let tool = SkillCreateTool::new(tmp.path().to_path_buf(), installer, empty_snapshot());
        let _ = tool
            .execute(
                json!({
                    "name": "quoted",
                    "description": "Has \"quotes\" and \\ backslash",
                    "system_prompt": "p",
                }),
                ctx(),
            )
            .await
            .unwrap();
        let body = std::fs::read_to_string(tmp.path().join("quoted/SKILL.toml")).unwrap();
        // Parse what we wrote — the TOML must round-trip.
        let parsed: toml::Value = toml::from_str(&body).expect("valid TOML");
        let desc = parsed["skill"]["description"].as_str().unwrap();
        assert_eq!(desc, "Has \"quotes\" and \\ backslash");
    }
}
