use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TeamRole {
    Coordinator,
    Worker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub agent_name: String,
    pub role: TeamRole,
    #[serde(default)]
    pub role_description: String,
}

/// Declarative configuration for an agent team.
/// Can be loaded from a TOML file or constructed programmatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub name: String,
    pub goal: String,
    pub members: Vec<TeamMember>,
    /// Maximum coordinator↔worker rounds before the team is forced to conclude.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
}

fn default_max_rounds() -> u32 {
    10
}

impl TeamConfig {
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    pub fn coordinator(&self) -> Option<&TeamMember> {
        self.members
            .iter()
            .find(|m| matches!(m.role, TeamRole::Coordinator))
    }

    pub fn workers(&self) -> Vec<&TeamMember> {
        self.members
            .iter()
            .filter(|m| matches!(m.role, TeamRole::Worker))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_team_config_from_toml() {
        let toml = r#"
            name = "research-team"
            goal = "Research and summarize the topic"

            [[members]]
            agent_name = "planner"
            role = "Coordinator"
            role_description = "Breaks the goal into subtasks"

            [[members]]
            agent_name = "researcher"
            role = "Worker"
            role_description = "Searches and retrieves information"
        "#;

        let config = TeamConfig::from_toml(toml).unwrap();
        assert_eq!(config.name, "research-team");
        assert!(config.coordinator().is_some());
        assert_eq!(config.coordinator().unwrap().agent_name, "planner");
        assert_eq!(config.workers().len(), 1);
        assert_eq!(config.workers()[0].agent_name, "researcher");
        assert_eq!(config.max_rounds, 10);
    }

    #[test]
    fn test_team_config_no_coordinator_returns_none() {
        let toml = r#"
            name = "worker-only"
            goal = "Do work"

            [[members]]
            agent_name = "worker1"
            role = "Worker"
        "#;
        let config = TeamConfig::from_toml(toml).unwrap();
        assert!(config.coordinator().is_none());
        assert_eq!(config.workers().len(), 1);
    }
}
