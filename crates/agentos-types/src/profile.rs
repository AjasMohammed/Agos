use crate::ids::ProfileEntryID;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileCategory {
    CommunicationStyle,
    TechStack,
    Workflow,
    DomainInterest,
    Constraint,
    Other,
}

impl ProfileCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileCategory::CommunicationStyle => "communication_style",
            ProfileCategory::TechStack => "tech_stack",
            ProfileCategory::Workflow => "workflow",
            ProfileCategory::DomainInterest => "domain_interest",
            ProfileCategory::Constraint => "constraint",
            ProfileCategory::Other => "other",
        }
    }
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "communication_style" => ProfileCategory::CommunicationStyle,
            "tech_stack" => ProfileCategory::TechStack,
            "workflow" => ProfileCategory::Workflow,
            "domain_interest" => ProfileCategory::DomainInterest,
            "constraint" => ProfileCategory::Constraint,
            _ => ProfileCategory::Other,
        }
    }
}

/// Provenance of a profile entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProfileSource {
    FromProposal { proposal_id: String },
    Explicit,
    Inferred,
}

impl ProfileSource {
    /// Discriminant stored in the `source_kind` column.
    pub fn discriminant(&self) -> &'static str {
        match self {
            ProfileSource::FromProposal { .. } => "from_proposal",
            ProfileSource::Explicit => "explicit",
            ProfileSource::Inferred => "inferred",
        }
    }
    /// The `source_ref` column value (proposal id when from a proposal).
    pub fn source_ref(&self) -> Option<&str> {
        match self {
            ProfileSource::FromProposal { proposal_id } => Some(proposal_id),
            _ => None,
        }
    }
    pub fn from_parts(kind: &str, source_ref: Option<String>) -> Self {
        match kind {
            "from_proposal" => ProfileSource::FromProposal {
                proposal_id: source_ref.unwrap_or_default(),
            },
            "inferred" => ProfileSource::Inferred,
            _ => ProfileSource::Explicit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileEntryStatus {
    Active,
    Archived,
}

impl ProfileEntryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileEntryStatus::Active => "active",
            ProfileEntryStatus::Archived => "archived",
        }
    }
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "archived" => ProfileEntryStatus::Archived,
            _ => ProfileEntryStatus::Active,
        }
    }
}

/// A single structured user-profile fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub id: ProfileEntryID,
    pub category: ProfileCategory,
    /// Stable slug key, e.g. "preferred_language" or "tone".
    pub key: String,
    /// Human-readable value, e.g. "Rust" or "concise, no preamble".
    pub value: String,
    /// 0.0..=1.0; entries below the floor are rejected by the store.
    pub confidence: f32,
    pub source: ProfileSource,
    /// L0 pin ordering; lower = higher priority. Non-pinned uses UNPINNED_RANK.
    pub pin_rank: i64,
    pub usage_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default = "default_status")]
    pub status: ProfileEntryStatus,
}

fn default_status() -> ProfileEntryStatus {
    ProfileEntryStatus::Active
}

/// Partial update applied to an existing [`ProfileEntry`] by the store.
#[derive(Debug, Clone, Default)]
pub struct ProfilePatch {
    pub category: Option<ProfileCategory>,
    pub value: Option<String>,
    pub confidence: Option<f32>,
    pub pin_rank: Option<i64>,
    pub status: Option<ProfileEntryStatus>,
}
