//! DTOs for the marketplace proxy (a thin pass-through to the external tool
//! registry at `AGENTOS_REGISTRY_URL`). Responses are the registry's JSON,
//! returned verbatim under the standard envelope.

use serde::{Deserialize, Serialize};

/// Query parameters for marketplace search.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MarketplaceQuery {
    /// Free-text search query.
    pub q: Option<String>,
    /// Optional artifact-type filter (e.g. `tool`, `plugin`).
    #[serde(rename = "type")]
    pub artifact_type: Option<String>,
}

/// Request body for submitting a marketplace review.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubmitReviewRequest {
    /// Rating, 1–5.
    pub rating: u8,
    pub comment: String,
    /// Public key id of the reviewer (non-secret).
    pub author_key: String,
}
