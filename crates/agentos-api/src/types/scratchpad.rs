//! DTOs for the agent scratchpad REST surface (Phase 06).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Lightweight scratchpad page summary. Mirrors `agentos_scratch::PageSummary`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiPageSummary {
    /// Page id (UUID).
    pub id: String,
    /// Page title (also the lookup key).
    pub title: String,
    /// Tags attached to the page.
    pub tags: Vec<String>,
    /// RFC3339 last-updated timestamp.
    pub updated_at: String,
}

/// A full scratchpad page plus its backlinks. Mirrors `agentos_scratch::ScratchPage`
/// with the backlink summaries from `get_all_links`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiScratchPage {
    /// Page id (UUID).
    pub id: String,
    /// Owning agent id (the reserved `__global__` sentinel for global pages).
    pub agent_id: String,
    /// Page title.
    pub title: String,
    /// Markdown body.
    pub content: String,
    /// Tags attached to the page.
    pub tags: Vec<String>,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-updated timestamp.
    pub updated_at: String,
    /// Pages that link to this page (backlink graph).
    pub backlinks: Vec<ApiPageSummary>,
}

/// Request body for `PUT /api/v1/scratchpad/{page}` (and the per-agent variant).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SavePageRequest {
    /// Markdown body to write.
    pub content: String,
    /// Tags to attach (defaults to empty).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Response wrapper for scratchpad list endpoints.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScratchListResponse {
    /// Page summaries for the scratchpad.
    pub pages: Vec<ApiPageSummary>,
}
