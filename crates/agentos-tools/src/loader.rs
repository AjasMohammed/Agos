use crate::signing::verify_manifest;
use agentos_types::{AgentOSError, ToolManifest};
use std::path::{Path, PathBuf};

/// A loaded manifest and the directory it lives in (needed to resolve relative wasm_path).
pub struct LoadedManifest {
    pub manifest: ToolManifest,
    /// Directory containing the `.toml` file — used to resolve relative `wasm_path`.
    pub manifest_dir: PathBuf,
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

    let manifest: ToolManifest = toml::from_str(&content).map_err(|e| {
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
            manifests.push(load_manifest(&path)?);
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
}
