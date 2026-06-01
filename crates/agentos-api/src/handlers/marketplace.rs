//! Marketplace proxy — a thin pass-through to the external tool registry at
//! `AGENTOS_REGISTRY_URL` (default `http://localhost:8090`). Degrades gracefully
//! (empty list / 404 / 503) when the registry is unavailable. Registry creds /
//! URL are never echoed into responses.

use axum::extract::{Path, Query};
use axum::{Extension, Json};

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::types::{MarketplaceQuery, SubmitReviewRequest};

fn registry_base_url() -> String {
    std::env::var("AGENTOS_REGISTRY_URL").unwrap_or_else(|_| "http://localhost:8090".to_string())
}

/// A registry HTTP client that does NOT follow redirects. A malicious/compromised
/// registry could otherwise 3xx us to an internal address (SSRF); with redirects
/// disabled a 3xx surfaces as an error and we fail closed.
fn registry_agent() -> ureq::Agent {
    ureq::AgentBuilder::new().redirects(0).build()
}

/// Minimal percent-encoding for path/query segments (unreserved chars pass through).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `GET /api/v1/marketplace?q=&type=` — Search the registry (empty list on failure).
#[utoipa::path(
    get, path = "/api/v1/marketplace", tag = "marketplace", operation_id = "marketplace_search",
    params(MarketplaceQuery),
    responses(
        (status = 200, description = "Registry search results (verbatim registry JSON)", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn search(
    Extension(key): Extension<AuthenticatedKey>,
    Query(q): Query<MarketplaceQuery>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "marketplace:r")?;
    let base = registry_base_url();
    let mut url = format!("{base}/v1/tools?limit=50");
    if let Some(query) = q.q.as_deref().filter(|s| !s.is_empty()) {
        url.push_str(&format!("&q={}", enc(query)));
    }

    let json = tokio::task::spawn_blocking(move || {
        let resp = registry_agent().get(&url).call().ok()?;
        let body = resp.into_string().ok()?;
        serde_json::from_str::<serde_json::Value>(&body).ok()
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| serde_json::json!([]));

    // Optional artifact-type filter applied client-side.
    let filtered = match (q.artifact_type.as_deref(), json.as_array()) {
        (Some(t), Some(arr)) if !t.is_empty() => serde_json::Value::Array(
            arr.iter()
                .filter(|i| {
                    i.get("artifact_type")
                        .and_then(|v| v.as_str())
                        .map(|x| x == t)
                        .unwrap_or(true)
                })
                .cloned()
                .collect(),
        ),
        _ => json,
    };
    Ok(Json(Envelope::new(filtered)))
}

/// `GET /api/v1/marketplace/{name}` — Fetch a single registry item.
#[utoipa::path(
    get, path = "/api/v1/marketplace/{name}", tag = "marketplace", operation_id = "marketplace_detail",
    params(("name" = String, Path, description = "Registry item name")),
    responses(
        (status = 200, description = "Registry item detail (verbatim registry JSON)", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Not found or registry unavailable", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn detail(
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "marketplace:r")?;
    let base = registry_base_url();
    let url = format!("{base}/v1/tools/{}", enc(&name));
    let json = tokio::task::spawn_blocking(move || {
        let resp = registry_agent().get(&url).call().ok()?;
        let body = resp.into_string().ok()?;
        serde_json::from_str::<serde_json::Value>(&body).ok()
    })
    .await
    .ok()
    .flatten();

    match json {
        Some(j) => Ok(Json(Envelope::new(j))),
        None => Err(ApiError::NotFound(format!(
            "Marketplace item '{name}' not found (or registry unavailable)"
        ))),
    }
}

/// `POST /api/v1/marketplace/{name}/reviews` — Submit a review to the registry.
#[utoipa::path(
    post, path = "/api/v1/marketplace/{name}/reviews", tag = "marketplace", operation_id = "marketplace_review",
    params(("name" = String, Path, description = "Registry item name")),
    request_body = SubmitReviewRequest,
    responses(
        (status = 200, description = "Review submitted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 503, description = "Registry unavailable", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn review(
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
    Json(req): Json<SubmitReviewRequest>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "marketplace:w")?;
    let base = registry_base_url();
    let url = format!("{base}/v1/tools/{}/reviews", enc(&name));
    let body = serde_json::to_string(&req).map_err(|e| ApiError::Internal(e.to_string()))?;
    // Distinguish a transport failure (registry down → 503) from a rejection by the
    // registry (HTTP 4xx → 400, HTTP 5xx → 503) so we don't report a client error
    // as an outage. `Err(Some(code))` carries the registry's status.
    let outcome: Result<(), Option<u16>> =
        tokio::task::spawn_blocking(move || {
            match registry_agent()
                .post(&url)
                .set("content-type", "application/json")
                .send_string(&body)
            {
                Ok(_) => Ok(()),
                Err(ureq::Error::Status(code, _)) => Err(Some(code)),
                Err(ureq::Error::Transport(_)) => Err(None),
            }
        })
        .await
        .unwrap_or(Err(None));

    match outcome {
        Ok(()) => Ok(Json(Envelope::new(serde_json::json!({ "ok": true })))),
        Err(Some(code)) if (400..500).contains(&code) => Err(ApiError::BadRequest(format!(
            "registry rejected review (HTTP {code})"
        ))),
        Err(_) => Err(ApiError::ServiceUnavailable(
            "marketplace registry unavailable".into(),
        )),
    }
}
