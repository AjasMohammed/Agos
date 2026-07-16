//! DTOs for the read-only agent-memory browser.
//!
//! One normalized item shape spans all three tiers (episodic / semantic /
//! procedural) so the panel renders a single list; `tier` + `kind` carry the
//! provenance and `metadata` the tier-specific extras.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Query for `GET /api/v1/agents/{id}/memory/{tier}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MemoryQuery {
    /// Search query. When empty, returns the most-recent items (browse mode).
    pub q: Option<String>,
    /// Max items to return (default 50, capped at 200).
    pub limit: Option<usize>,
}

/// A single memory item, normalized across all three tiers.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiMemoryItem {
    /// Tier-local id (episodic rowid, semantic/procedural UUID) as a string.
    pub id: String,
    /// `episodic` | `semantic` | `procedural`.
    pub tier: String,
    /// Sub-kind: the episodic entry type, `fact`, or `procedure`.
    pub kind: String,
    /// Short label (episodic summary, semantic key, procedure name).
    pub title: String,
    /// Body text (episodic content, semantic fact, procedure description).
    pub content: String,
    /// Creation time — except procedural items, which carry `updated_at` so the
    /// timestamp matches that tier's recency ordering.
    pub created_at: DateTime<Utc>,
    /// Search relevance (RRF fused score) when `q` was supplied; `null` in browse mode.
    pub score: Option<f32>,
    /// Tier-specific extras (tags, use_count, success/failure counts, …).
    pub metadata: serde_json::Value,
}
