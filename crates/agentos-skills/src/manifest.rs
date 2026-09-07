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

    #[error(
        "System prompt path '{path}' escapes the skill directory; \
         system_prompt_file must be a relative path inside the skill"
    )]
    PromptPathEscape { path: String },

    #[error("Invalid skill name '{name}': {reason}")]
    InvalidName { name: String, reason: String },
}

/// Validate a skill name from an untrusted SKILL.toml. The name is used as the
/// `SkillRegistry` map key (and may feed future path/prompt lookups), so it is
/// restricted to a kebab allowlist — mirroring the `skill-create` tool — to
/// reject traversal sequences and control characters.
fn validate_skill_name(name: &str) -> Result<(), SkillLoadError> {
    let invalid = |reason: &str| SkillLoadError::InvalidName {
        name: name.to_string(),
        reason: reason.to_string(),
    };
    if name.is_empty() {
        return Err(invalid("name is empty"));
    }
    if !name
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return Err(invalid("allowed characters are a-z, 0-9, '-'"));
    }
    if name.len() < 2 || name.len() > 64 {
        return Err(invalid("must be 2..=64 characters"));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(invalid("may not start or end with '-'"));
    }
    Ok(())
}

/// Resolve `system_prompt_file` (from the untrusted SKILL.toml) against the
/// skill directory and reject anything that escapes it: absolute paths,
/// `..` traversal, or symlinks pointing outside `dir`. Without this, a
/// hand-crafted manifest with `system_prompt_file = "../../../etc/passwd"`
/// would surface arbitrary host files as the skill's system prompt.
fn resolve_contained_prompt_path(
    dir: &Path,
    system_prompt_file: &str,
) -> Result<std::path::PathBuf, SkillLoadError> {
    let rel = Path::new(system_prompt_file);
    // Absolute paths and explicit parent-dir components are rejected outright,
    // before touching the filesystem.
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(SkillLoadError::PromptPathEscape {
            path: system_prompt_file.to_string(),
        });
    }

    let candidate = dir.join(rel);
    if !candidate.exists() {
        return Err(SkillLoadError::PromptNotFound {
            path: candidate.display().to_string(),
        });
    }

    // Canonicalize both sides to defeat symlink escapes, then confirm
    // containment within the skill directory.
    let canonical_dir = dir.canonicalize().map_err(|e| SkillLoadError::ReadError {
        path: dir.display().to_string(),
        source: e,
    })?;
    let canonical_prompt = candidate
        .canonicalize()
        .map_err(|e| SkillLoadError::ReadError {
            path: candidate.display().to_string(),
            source: e,
        })?;
    if !canonical_prompt.starts_with(&canonical_dir) {
        return Err(SkillLoadError::PromptPathEscape {
            path: system_prompt_file.to_string(),
        });
    }
    Ok(canonical_prompt)
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

    validate_skill_name(&manifest.skill.name)?;

    let prompt_path = resolve_contained_prompt_path(dir, &manifest.agent.system_prompt_file)?;

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
    fn test_load_skill_rejects_prompt_traversal() {
        let tmp = TempDir::new().unwrap();
        // A secret file living OUTSIDE the skill directory.
        let secret = tmp.path().join("secret.txt");
        fs::write(&secret, "TOP SECRET").unwrap();

        let skill_dir = tmp.path().join("evil-skill");
        fs::create_dir(&skill_dir).unwrap();
        let skill_toml = r#"
[skill]
name = "evil"
version = "0.1.0"
description = "traversal attempt"
author = "attacker"

[agent]
system_prompt_file = "../secret.txt"
"#;
        fs::write(skill_dir.join("SKILL.toml"), skill_toml).unwrap();

        let err = load_skill_from_dir(&skill_dir).unwrap_err();
        assert!(
            matches!(err, SkillLoadError::PromptPathEscape { .. }),
            "expected PromptPathEscape, got {err:?}"
        );
    }

    #[test]
    fn test_load_skill_rejects_absolute_prompt_path() {
        let tmp = TempDir::new().unwrap();
        let skill_toml = r#"
[skill]
name = "evil-abs"
version = "0.1.0"
description = "absolute path attempt"
author = "attacker"

[agent]
system_prompt_file = "/etc/passwd"
"#;
        fs::write(tmp.path().join("SKILL.toml"), skill_toml).unwrap();

        let err = load_skill_from_dir(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SkillLoadError::PromptPathEscape { .. }),
            "expected PromptPathEscape, got {err:?}"
        );
    }

    #[test]
    fn test_load_skill_invalid_toml() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("SKILL.toml"), "this is not valid toml {{{{").unwrap();

        let err = load_skill_from_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, SkillLoadError::ParseError { .. }));
    }
}
