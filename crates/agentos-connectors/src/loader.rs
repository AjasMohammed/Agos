use crate::definition::ConnectorManifest;
use agentos_types::AgentOSError;
use std::path::Path;

/// Load all connector manifests from a directory.
///
/// Each `.toml` file in the directory is parsed as a `ConnectorManifest`.
/// Files that fail to parse are logged and skipped (not fatal).
pub fn load_connector_manifests(dir: &Path) -> Result<Vec<ConnectorManifest>, AgentOSError> {
    if !dir.exists() {
        tracing::debug!(path = %dir.display(), "Connector directory does not exist, skipping");
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(dir).map_err(|e| AgentOSError::KernelError {
        reason: format!("Failed to read connector directory {}: {e}", dir.display()),
    })?;

    let mut manifests = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read directory entry");
                continue;
            }
        };

        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }

        match load_single_manifest(&path) {
            Ok(manifest) => {
                tracing::info!(
                    connector = %manifest.connector.id,
                    tools = manifest.tools.len(),
                    path = %path.display(),
                    "Loaded connector manifest"
                );
                manifests.push(manifest);
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to load connector manifest, skipping"
                );
            }
        }
    }

    Ok(manifests)
}

/// Load a single connector manifest from a TOML file.
pub fn load_single_manifest(path: &Path) -> Result<ConnectorManifest, AgentOSError> {
    let content = std::fs::read_to_string(path).map_err(|e| AgentOSError::KernelError {
        reason: format!("Failed to read connector file {}: {e}", path.display()),
    })?;

    let manifest: ConnectorManifest =
        toml::from_str(&content).map_err(|e| AgentOSError::KernelError {
            reason: format!("Failed to parse connector TOML {}: {e}", path.display()),
        })?;

    // Basic validation
    if manifest.connector.id.is_empty() {
        return Err(AgentOSError::KernelError {
            reason: format!("Connector in {} has empty id", path.display()),
        });
    }

    if manifest.connector.base_url.is_empty() {
        return Err(AgentOSError::KernelError {
            reason: format!("Connector '{}' has empty base_url", manifest.connector.id),
        });
    }

    // Validate tool names don't contain dots (they're used as namespace separators)
    for tool in &manifest.tools {
        if tool.name.contains('.') {
            return Err(AgentOSError::KernelError {
                reason: format!(
                    "Tool name '{}' in connector '{}' must not contain dots",
                    tool.name, manifest.connector.id
                ),
            });
        }
    }

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_manifests_from_dir() {
        let tmp = TempDir::new().unwrap();

        // Write a valid manifest
        std::fs::write(
            tmp.path().join("test.toml"),
            r#"
[connector]
id = "test"
name = "Test"
version = "1.0.0"
description = "Test connector"
base_url = "https://api.example.com"

[connector.auth]
type = "none"

[[tools]]
name = "hello"
description = "Say hello"
method = "get"
path = "/hello"
"#,
        )
        .unwrap();

        // Write an invalid file (should be skipped, not fatal)
        std::fs::write(tmp.path().join("bad.toml"), "not valid toml {{{{").unwrap();

        // Write a non-TOML file (should be ignored)
        std::fs::write(tmp.path().join("readme.md"), "# Ignore me").unwrap();

        let manifests = load_connector_manifests(tmp.path()).unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].connector.id, "test");
    }

    #[test]
    fn test_load_nonexistent_dir() {
        let manifests = load_connector_manifests(Path::new("/nonexistent/dir")).unwrap();
        assert!(manifests.is_empty());
    }

    #[test]
    fn test_validate_empty_id() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bad.toml");
        std::fs::write(
            &path,
            r#"
[connector]
id = ""
name = "Bad"
version = "1.0.0"
description = "Bad connector"
base_url = "https://example.com"
[connector.auth]
type = "none"
"#,
        )
        .unwrap();

        assert!(load_single_manifest(&path).is_err());
    }

    #[test]
    fn test_validate_dot_in_tool_name() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bad.toml");
        std::fs::write(
            &path,
            r#"
[connector]
id = "test"
name = "Test"
version = "1.0.0"
description = "Test"
base_url = "https://example.com"
[connector.auth]
type = "none"
[[tools]]
name = "bad.name"
description = "Has a dot"
method = "get"
path = "/bad"
"#,
        )
        .unwrap();

        let err = load_single_manifest(&path).unwrap_err();
        assert!(err.to_string().contains("must not contain dots"));
    }
}
