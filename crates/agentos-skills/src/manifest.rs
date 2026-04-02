use agentos_types::skill::SkillManifest;
use std::path::Path;

/// Errors that can occur when loading a skill.
#[derive(Debug, thiserror::Error)]
pub enum SkillLoadError {
    #[error("SKILL.toml not found in '{0}'")]
    ManifestNotFound(String),

    #[error("Failed to read '{path}': {source}")]
    ReadError {
        path: String,
        source: std::io::Error,
    },

    #[error("Invalid SKILL.toml in '{path}': {source}")]
    ParseError {
        path: String,
        source: toml::de::Error,
    },

    #[error("System prompt file '{path}' not found (referenced by SKILL.toml)")]
    PromptNotFound { path: String },
}

/// Load a skill from a directory containing `SKILL.toml` and the referenced
/// system prompt file.
///
/// Returns the parsed manifest and the system prompt content as a string.
pub fn load_skill_from_dir(dir: &Path) -> Result<(SkillManifest, String), SkillLoadError> {
    let manifest_path = dir.join("SKILL.toml");
    if !manifest_path.exists() {
        return Err(SkillLoadError::ManifestNotFound(dir.display().to_string()));
    }

    let manifest_content =
        std::fs::read_to_string(&manifest_path).map_err(|e| SkillLoadError::ReadError {
            path: manifest_path.display().to_string(),
            source: e,
        })?;

    let manifest: SkillManifest =
        toml::from_str(&manifest_content).map_err(|e| SkillLoadError::ParseError {
            path: manifest_path.display().to_string(),
            source: e,
        })?;

    let prompt_path = dir.join(&manifest.agent.system_prompt_file);
    if !prompt_path.exists() {
        return Err(SkillLoadError::PromptNotFound {
            path: prompt_path.display().to_string(),
        });
    }

    let prompt_content =
        std::fs::read_to_string(&prompt_path).map_err(|e| SkillLoadError::ReadError {
            path: prompt_path.display().to_string(),
            source: e,
        })?;

    Ok((manifest, prompt_content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_skill(dir: &Path) {
        let skill_toml = r#"
[skill]
name = "test-skill"
version = "0.1.0"
description = "A test skill for unit tests"
author = "test-author"

[agent]
system_prompt_file = "prompt.md"
roles = ["tester"]

[tools]
required = ["file-read"]
optional = ["shell-exec"]

[permissions]
required = ["fs.user_data:r"]

[budget]
max_cost_per_run = 0.25
max_tokens_per_run = 25000
"#;
        fs::write(dir.join("SKILL.toml"), skill_toml).unwrap();
        fs::write(dir.join("prompt.md"), "You are a test skill agent.").unwrap();
    }

    #[test]
    fn test_load_skill_from_dir_success() {
        let tmp = TempDir::new().unwrap();
        create_test_skill(tmp.path());

        let (manifest, prompt) = load_skill_from_dir(tmp.path()).unwrap();
        assert_eq!(manifest.skill.name, "test-skill");
        assert_eq!(manifest.skill.version, "0.1.0");
        assert_eq!(manifest.skill.author, "test-author");
        assert_eq!(manifest.agent.system_prompt_file, "prompt.md");
        assert_eq!(manifest.tools.required, vec!["file-read"]);
        assert_eq!(prompt, "You are a test skill agent.");
    }

    #[test]
    fn test_load_skill_missing_manifest() {
        let tmp = TempDir::new().unwrap();
        let err = load_skill_from_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, SkillLoadError::ManifestNotFound(_)));
    }

    #[test]
    fn test_load_skill_missing_prompt() {
        let tmp = TempDir::new().unwrap();
        let skill_toml = r#"
[skill]
name = "no-prompt"
version = "0.1.0"
description = "Missing prompt file"
author = "test"

[agent]
system_prompt_file = "nonexistent.md"
"#;
        fs::write(tmp.path().join("SKILL.toml"), skill_toml).unwrap();

        let err = load_skill_from_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, SkillLoadError::PromptNotFound { .. }));
    }

    #[test]
    fn test_load_skill_invalid_toml() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("SKILL.toml"), "this is not valid toml {{{{").unwrap();

        let err = load_skill_from_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, SkillLoadError::ParseError { .. }));
    }
}
