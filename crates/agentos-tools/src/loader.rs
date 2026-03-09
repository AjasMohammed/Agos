use agentos_types::{AgentOSError, ToolManifest};
use std::path::{Path, PathBuf};

/// A loaded manifest and the directory it lives in (needed to resolve relative wasm_path).
pub struct LoadedManifest {
    pub manifest: ToolManifest,
    /// Directory containing the `.toml` file — used to resolve relative `wasm_path`.
    pub manifest_dir: PathBuf,
}

/// Load a ToolManifest from a TOML file.
pub fn load_manifest(path: &Path) -> Result<LoadedManifest, AgentOSError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        AgentOSError::ToolNotFound(format!("Cannot read manifest {:?}: {}", path, e))
    })?;

    let manifest: ToolManifest = toml::from_str(&content).map_err(|e| {
        AgentOSError::SchemaValidation(format!("Invalid manifest {:?}: {}", path, e))
    })?;

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
