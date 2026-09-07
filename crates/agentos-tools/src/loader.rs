use crate::signing::verify_manifest;
use agentos_types::{AgentOSError, ToolManifest};
use std::path::{Path, PathBuf};

/// A loaded manifest and the directory it lives in (needed to resolve relative wasm_path).
pub struct LoadedManifest {
    pub manifest: ToolManifest,
    /// Directory containing the `.toml` file — used to resolve relative `wasm_path`.
    pub manifest_dir: PathBuf,
}

/// Parse a `ToolManifest` from TOML text.
///
/// `risk_class` is a *top-level* field of [`ToolManifest`], but it is easy to
/// author it under `[manifest]` or `[sandbox]` by mistake. serde silently
/// drops unknown keys there, so the field would fall back to the default.
/// The default is now `ExecCapable` (fail-closed), but a misplaced
/// `readonly_external` silently becoming `ExecCapable` is still wrong —
/// the declared class must be honoured. Hoist a nested `risk_class` to the top level (when the top
/// level has none) before deserializing, so every loader path agrees.
pub fn parse_manifest(content: &str) -> Result<ToolManifest, AgentOSError> {
    let mut value: toml::Value = toml::from_str(content)
        .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid manifest TOML: {e}")))?;

    if let Some(table) = value.as_table_mut() {
        if !table.contains_key("risk_class") {
            // If both sections declare one, keep the MOST restrictive so a
            // conflicting manifest can never talk its way down a tier.
            let nested = ["manifest", "sandbox"]
                .iter()
                .filter_map(|section| {
                    table
                        .get(*section)
                        .and_then(|v| v.get("risk_class"))
                        .cloned()
                        .map(|rc| (*section, rc))
                })
                .max_by_key(|(_, rc)| risk_rank(rc.as_str().unwrap_or("")));
            if let Some((section, rc)) = nested {
                let name = table
                    .get("manifest")
                    .and_then(|m| m.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("<unknown>")
                    .to_string();
                // A value we don't recognise must never silently fall back to
                // the default, so reject the manifest — but say
                // exactly why, instead of letting serde emit a cryptic error
                // about a field the author wrote in the wrong section.
                if rc
                    .as_str()
                    .is_none_or(|s| risk_rank(s) >= UNKNOWN_RISK_RANK)
                {
                    return Err(AgentOSError::SchemaValidation(format!(
                        "manifest '{name}': [{section}].risk_class = {rc} is not a known \
                         risk class (readonly_scoped, readonly_external, write_agent_state, \
                         write_scoped, \
                         exec_capable, interactive, control_plane)"
                    )));
                }
                tracing::warn!(
                    tool = name,
                    section,
                    "manifest declares risk_class under [{section}]; hoisting to top level"
                );
                table.insert("risk_class".to_string(), rc);
            }
        }
    }

    value
        .try_into::<ToolManifest>()
        .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid manifest: {e}")))
}

/// Rank returned for a value that is not a known risk class. Sorts highest so
/// a typo is never *preferred over* a real class, and is rejected outright by
/// `parse_manifest` rather than silently defaulting.
const UNKNOWN_RISK_RANK: u8 = 7;

/// Ordering used to pick the stricter of two declared risk classes.
fn risk_rank(s: &str) -> u8 {
    match s {
        "readonly_scoped" => 0,
        "readonly_external" => 1,
        "write_agent_state" => 2,
        "write_scoped" => 3,
        "exec_capable" => 4,
        "interactive" => 5,
        "control_plane" => 6,
        _ => UNKNOWN_RISK_RANK,
    }
}

/// Load a ToolManifest from a TOML file and verify its trust-tier signature.
///
/// Returns an error if:
/// - The file cannot be read or parsed.
/// - The manifest has `trust_tier = "blocked"`.
/// - The manifest has `trust_tier = "community"` or `"verified"` but the
///   Ed25519 signature is absent or does not match the signing payload.
pub fn load_manifest(path: &Path) -> Result<LoadedManifest, AgentOSError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        AgentOSError::ToolNotFound(format!("Cannot read manifest {:?}: {}", path, e))
    })?;

    let manifest = parse_manifest(&content).map_err(|e| {
        AgentOSError::SchemaValidation(format!("Invalid manifest {:?}: {}", path, e))
    })?;

    // Enforce trust tier policy before accepting the manifest.
    verify_manifest(&manifest)?;

    let manifest_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

    Ok(LoadedManifest {
        manifest,
        manifest_dir,
    })
}

/// Load all manifests from a directory.
pub fn load_all_manifests(dir: &Path) -> Result<Vec<LoadedManifest>, AgentOSError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| AgentOSError::ToolExecutionFailed {
        tool_name: "loader".into(),
        reason: format!("Cannot read tools directory {:?}: {}", dir, e),
    })? {
        let entry = entry.map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "loader".into(),
            reason: format!("Error reading directory entry: {}", e),
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            // One bad or unverifiable manifest must not take the whole
            // directory (and the kernel boot) down with it.
            match load_manifest(&path) {
                Ok(m) => manifests.push(m),
                // A blocked tier or a bad signature is a security event, not a
                // typo: log it loudly and distinctly so it is visible in the
                // kernel log / `GET /api/v1/logs` rather than blending into
                // ordinary parse noise.
                Err(
                    e @ (AgentOSError::ToolBlocked { .. }
                    | AgentOSError::ToolSignatureInvalid { .. }),
                ) => tracing::error!(
                    path = %path.display(),
                    error = %e,
                    security = true,
                    "Refusing tool manifest: trust-tier verification failed"
                ),
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Skipping tool manifest that failed to load"
                ),
            }
        }
    }
    Ok(manifests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{ExecutorType, RiskClass, TrustTier};

    /// Round-trip the shipped `tools/core/host-package-install.toml` manifest.
    /// Catches the entire class of bug where `risk_class` (or any other
    /// top-level `ToolManifest` field) is silently absorbed into a nested
    /// section by serde because `deny_unknown_fields` is not set.
    #[test]
    fn host_package_install_manifest_parses_with_correct_top_level_fields() {
        // Resolve workspace root: the test runs from
        // crates/agentos-tools/, so ../../tools/core/host-package-install.toml.
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/core/host-package-install.toml");
        let loaded = load_manifest(&path).expect("manifest must parse and verify");

        assert_eq!(loaded.manifest.manifest.name, "host-package-install");
        assert_eq!(loaded.manifest.manifest.trust_tier, TrustTier::Core);
        assert_eq!(
            loaded.manifest.risk_class,
            RiskClass::ControlPlane,
            "risk_class MUST be at the top level of the TOML; if it is nested \
             inside [manifest] serde silently defaults the outer field to \
             ReadonlyScoped and the privileged-executor gate rejects the tool"
        );
        assert_eq!(
            loaded.manifest.executor.executor_type,
            ExecutorType::Privileged
        );
    }

    /// Round-trip the shipped `tools/core/skill-create.toml` manifest. The
    /// `skill-create` tool lets an agent author + install skills at runtime,
    /// so its `risk_class` MUST resolve to `ControlPlane` for the approval
    /// hook to gate every call. If `risk_class` is accidentally nested inside
    /// `[manifest]`, serde silently defaults the top-level field to
    /// `ReadonlyScoped` and the gate disappears — this test fails loudly.
    #[test]
    fn skill_create_manifest_parses_with_control_plane_risk_class() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/core/skill-create.toml");
        let loaded = load_manifest(&path).expect("manifest must parse and verify");

        assert_eq!(loaded.manifest.manifest.name, "skill-create");
        assert_eq!(loaded.manifest.manifest.trust_tier, TrustTier::Core);
        assert_eq!(
            loaded.manifest.risk_class,
            RiskClass::ControlPlane,
            "skill-create risk_class MUST be top-level ControlPlane so the \
             approval hook gates every skill-authoring call"
        );
    }
    #[test]
    fn parse_manifest_hoists_nested_risk_class() {
        let nested = r#"
[manifest]
name = "t"
version = "1.0.0"
description = "d"
author = "a"
trust_tier = "core"
risk_class = "exec_capable"

[capabilities_required]
permissions = []

[capabilities_provided]
outputs = []

[intent_schema]
input = "TIntent"
output = "TResult"

[sandbox]
network = false
fs_write = false
max_memory_mb = 4
max_cpu_ms = 100
"#;
        let m = parse_manifest(nested).expect("parses");
        assert_eq!(
            m.risk_class,
            RiskClass::ExecCapable,
            "nested risk_class must be hoisted"
        );

        let top = nested.replace("risk_class = \"exec_capable\"\n", "")
            + "\nrisk_class = \"control_plane\"\n";
        let m = parse_manifest(&top).expect("parses");
        assert_eq!(m.risk_class, RiskClass::ControlPlane);

        // A manifest that declares no risk_class at all must fail CLOSED.
        let none = nested.replace("risk_class = \"exec_capable\"\n", "");
        assert_eq!(
            parse_manifest(&none).unwrap().risk_class,
            RiskClass::ExecCapable
        );
    }

    /// Every shipped core manifest that declares a privileged `risk_class`
    /// must actually resolve to it — this is the gate the ApprovalHook relies on.
    #[test]
    fn every_core_manifest_resolves_declared_risk_class() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/core");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("tools/core exists") {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let Some(declared) = text
                .lines()
                .find_map(|l| l.trim().strip_prefix("risk_class"))
                .and_then(|rest| rest.split('"').nth(1))
            else {
                continue;
            };
            let manifest =
                parse_manifest(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let expected = match declared {
                "readonly_scoped" => RiskClass::ReadonlyScoped,
                "readonly_external" => RiskClass::ReadonlyExternal,
                "write_agent_state" => RiskClass::WriteAgentState,
                "write_scoped" => RiskClass::WriteScoped,
                "exec_capable" => RiskClass::ExecCapable,
                "control_plane" => RiskClass::ControlPlane,
                "interactive" => RiskClass::Interactive,
                other => panic!("{}: unknown risk_class {other}", path.display()),
            };
            assert_eq!(
                manifest.risk_class,
                expected,
                "{}: declared risk_class not honoured",
                path.display()
            );
            checked += 1;
        }
        assert!(
            checked >= 70,
            "expected most core manifests to declare risk_class, got {checked}"
        );
    }
}
