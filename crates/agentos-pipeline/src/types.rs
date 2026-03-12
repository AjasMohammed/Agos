use agentos_types::RunID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Tracks a single pipeline execution from start to finish.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRun {
    pub id: RunID,
    pub pipeline_name: String,
    pub input: String,
    pub status: PipelineRunStatus,
    pub step_results: HashMap<String, StepResult>,
    pub output: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PipelineRunStatus {
    Running,
    Complete,
    Failed,
    Cancelled,
}

impl std::fmt::Display for PipelineRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Complete => write!(f, "complete"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl PipelineRunStatus {
    pub fn parse_or_failed(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

impl std::str::FromStr for PipelineRunStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "complete" => Ok(Self::Complete),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("Unknown pipeline run status: {s}")),
        }
    }
}

/// Result of a single step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_id: String,
    pub status: StepStatus,
    pub output: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub attempt: u32,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Running,
    Complete,
    Failed,
    Skipped,
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Complete => write!(f, "complete"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

impl StepStatus {
    pub fn parse_or_failed(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            "skipped" => Self::Skipped,
            _ => Self::Failed,
        }
    }
}

impl std::str::FromStr for StepStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "complete" => Ok(Self::Complete),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            _ => Err(format!("Unknown step status: {s}")),
        }
    }
}
