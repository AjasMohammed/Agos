use serde::{Deserialize, Serialize};

/// A skill manifest describes an autonomous capability package.
///
/// Skills are higher-level than tools — they combine a system prompt,
/// tool set, trigger conditions, schedule, and budget into a single
/// deployable unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub skill: SkillInfo,
    #[serde(default)]
    pub triggers: SkillTriggers,
    pub agent: SkillAgent,
    #[serde(default)]
    pub tools: SkillTools,
    #[serde(default)]
    pub permissions: SkillPermissions,
    #[serde(default)]
    pub budget: SkillBudget,
}

/// Core metadata for a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default = "default_trust_tier")]
    pub trust_tier: String,
    #[serde(default)]
    pub license: Option<String>,
}

fn default_trust_tier() -> String {
    "community".to_string()
}

/// Trigger conditions that cause the skill to activate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillTriggers {
    /// Cron expression for scheduled activation.
    #[serde(default)]
    pub schedule: Option<String>,
    /// Event types that trigger the skill.
    #[serde(default)]
    pub events: Vec<String>,
}

/// Agent configuration for the skill's execution context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillAgent {
    /// Relative path to the system prompt markdown file within the skill directory.
    pub system_prompt_file: String,
    /// Roles the skill agent operates under.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Default LLM provider override (e.g. "openai", "anthropic", "ollama").
    #[serde(default)]
    pub default_provider: Option<String>,
    /// Default model override (e.g. "gpt-4o", "claude-3-opus").
    #[serde(default)]
    pub default_model: Option<String>,
}

/// Tool requirements for the skill.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillTools {
    /// Tools that must be available for the skill to function.
    #[serde(default)]
    pub required: Vec<String>,
    /// Tools the skill can use if available but are not mandatory.
    #[serde(default)]
    pub optional: Vec<String>,
}

/// Permission requirements for the skill.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillPermissions {
    /// Permissions the skill requires (e.g. "fs.user_data:rw").
    #[serde(default)]
    pub required: Vec<String>,
}

/// Budget constraints per skill execution run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillBudget {
    /// Maximum cost in USD per single run.
    #[serde(default = "default_max_cost")]
    pub max_cost_per_run: f64,
    /// Maximum tokens consumed per single run.
    #[serde(default = "default_max_tokens")]
    pub max_tokens_per_run: u64,
}

fn default_max_cost() -> f64 {
    1.0
}

fn default_max_tokens() -> u64 {
    100_000
}

impl Default for SkillBudget {
    fn default() -> Self {
        Self {
            max_cost_per_run: default_max_cost(),
            max_tokens_per_run: default_max_tokens(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_manifest_parse_minimal() {
        let toml_str = r#"
[skill]
name = "test-skill"
version = "0.1.0"
description = "A test skill"
author = "test"

[agent]
system_prompt_file = "prompt.md"
"#;
        let manifest: SkillManifest = toml::from_str(toml_str).expect("failed to parse manifest");
        assert_eq!(manifest.skill.name, "test-skill");
        assert_eq!(manifest.skill.version, "0.1.0");
        assert_eq!(manifest.skill.trust_tier, "community");
        assert_eq!(manifest.agent.system_prompt_file, "prompt.md");
        assert!(manifest.triggers.schedule.is_none());
        assert!(manifest.triggers.events.is_empty());
        assert!(manifest.tools.required.is_empty());
        assert!(manifest.permissions.required.is_empty());
        assert!((manifest.budget.max_cost_per_run - 1.0).abs() < f64::EPSILON);
        assert_eq!(manifest.budget.max_tokens_per_run, 100_000);
    }

    #[test]
    fn test_skill_manifest_parse_full() {
        let toml_str = r#"
[skill]
name = "code-reviewer"
version = "1.0.0"
description = "Autonomous code review skill"
author = "agentos"
trust_tier = "verified"
license = "MIT"

[triggers]
schedule = "0 */6 * * *"
events = ["TaskCompleted", "PipelineStageCompleted"]

[agent]
system_prompt_file = "prompt.md"
roles = ["reviewer", "analyst"]
default_provider = "anthropic"
default_model = "claude-sonnet-4-20250514"

[tools]
required = ["file-read", "shell-exec"]
optional = ["memory-search"]

[permissions]
required = ["fs.user_data:r", "process.exec:x"]

[budget]
max_cost_per_run = 0.50
max_tokens_per_run = 50000
"#;
        let manifest: SkillManifest = toml::from_str(toml_str).expect("failed to parse manifest");
        assert_eq!(manifest.skill.name, "code-reviewer");
        assert_eq!(manifest.skill.trust_tier, "verified");
        assert_eq!(manifest.skill.license, Some("MIT".to_string()));
        assert_eq!(manifest.triggers.schedule, Some("0 */6 * * *".to_string()));
        assert_eq!(manifest.triggers.events.len(), 2);
        assert_eq!(manifest.agent.roles.len(), 2);
        assert_eq!(
            manifest.agent.default_provider,
            Some("anthropic".to_string())
        );
        assert_eq!(manifest.tools.required.len(), 2);
        assert_eq!(manifest.tools.optional.len(), 1);
        assert_eq!(manifest.permissions.required.len(), 2);
        assert!((manifest.budget.max_cost_per_run - 0.50).abs() < f64::EPSILON);
        assert_eq!(manifest.budget.max_tokens_per_run, 50_000);
    }

    #[test]
    fn test_skill_budget_default() {
        let budget = SkillBudget::default();
        assert!((budget.max_cost_per_run - 1.0).abs() < f64::EPSILON);
        assert_eq!(budget.max_tokens_per_run, 100_000);
    }

    #[test]
    fn test_skill_roundtrip_serialize() {
        let toml_str = r#"
[skill]
name = "roundtrip"
version = "0.1.0"
description = "Roundtrip test"
author = "test"

[agent]
system_prompt_file = "prompt.md"
"#;
        let manifest: SkillManifest = toml::from_str(toml_str).expect("parse failed");
        let serialized = toml::to_string_pretty(&manifest).expect("serialize failed");
        let reparsed: SkillManifest = toml::from_str(&serialized).expect("reparse failed");
        assert_eq!(reparsed.skill.name, "roundtrip");
    }
}
