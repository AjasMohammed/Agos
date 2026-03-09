use serde::{Deserialize, Serialize};

/// A complete pipeline definition, deserialized from YAML.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PipelineDefinition {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub steps: Vec<PipelineStep>,
    /// Which output_var is the final result of the pipeline.
    #[serde(default)]
    pub output: Option<String>,
}

/// A step is either an agent task or a direct tool invocation — never both.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PipelineStep {
    pub id: String,
    #[serde(flatten)]
    pub action: StepAction,
    /// Variable name for this step's output.
    #[serde(default)]
    pub output_var: Option<String>,
    /// Step IDs this step depends on.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Enforced via tokio::time::timeout; step fails on expiry.
    #[serde(default)]
    pub timeout_minutes: Option<u64>,
    /// Max retries — engine re-runs the step up to N times on failure.
    #[serde(default)]
    pub retry_on_failure: Option<u32>,
    /// What to do when this step fails (after all retries exhausted). Default: Fail (stop pipeline).
    #[serde(default)]
    pub on_failure: OnFailure,
    /// Default value to use when on_failure is UseDefault.
    #[serde(default)]
    pub default_value: Option<String>,
}

/// Per-step failure handling strategy.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Stop the entire pipeline (default).
    #[default]
    Fail,
    /// Mark step as skipped and continue.
    Skip,
    /// Use `default_value` as the step's output and continue.
    UseDefault,
}

/// Exactly one of agent or tool must be specified per step.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StepAction {
    Agent {
        agent: String,
        task: String,
    },
    Tool {
        tool: String,
        input: serde_json::Value,
    },
}

impl PipelineDefinition {
    /// Parse a pipeline definition from a YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Serialize the definition to YAML.
    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}
