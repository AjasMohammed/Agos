use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::db::RegistryDb;
use crate::models::{ErrorResponse, PublishRequest, PublishResponse};

/// GET /v1/tools?q=<query>&limit=<n>&offset=<n>
#[derive(Deserialize)]
pub struct ListQuery {
    pub q: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    20
}

pub async fn list_tools(
    State(db): State<RegistryDb>,
    Query(params): Query<ListQuery>,
) -> impl IntoResponse {
    let result = if let Some(query) = &params.q {
        db.search(query, params.limit)
    } else {
        db.list_tools(params.limit, params.offset)
    };

    match result {
        Ok(tools) => Json(tools).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// GET /v1/tools/:name
pub async fn get_tool(State(db): State<RegistryDb>, Path(name): Path<String>) -> impl IntoResponse {
    match db.get_tool(&name) {
        Ok(Some(entry)) => Json(entry).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Tool '{}' not found", name),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// GET /v1/tools/:name/versions
pub async fn list_versions(
    State(db): State<RegistryDb>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match db.list_versions(&name) {
        Ok(versions) => Json(versions).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// GET /v1/tools/:name/:version
pub async fn get_tool_version(
    State(db): State<RegistryDb>,
    Path((name, version)): Path<(String, String)>,
) -> impl IntoResponse {
    match db.get_tool_version(&name, &version) {
        Ok(Some(entry)) => Json(entry).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Tool '{}' version '{}' not found", name, version),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// GET /v1/tools/:name/:version/dl — download manifest TOML and increment counter.
pub async fn download_tool(
    State(db): State<RegistryDb>,
    Path((name, version)): Path<(String, String)>,
) -> impl IntoResponse {
    match db.get_tool_version(&name, &version) {
        Ok(Some(entry)) => {
            let _ = db.increment_downloads(&name, &version);
            (
                StatusCode::OK,
                [("content-type", "application/toml")],
                entry.manifest_toml,
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Tool '{}' version '{}' not found", name, version),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// POST /v1/tools — publish a new tool version.
pub async fn publish_tool(
    State(db): State<RegistryDb>,
    Json(req): Json<PublishRequest>,
) -> impl IntoResponse {
    // Parse the manifest TOML.
    let manifest: agentos_types::ToolManifest = match toml::from_str(&req.manifest_toml) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("Invalid manifest TOML: {}", e),
                }),
            )
                .into_response();
        }
    };

    // Reject Core and Blocked trust tiers — only Verified and Community can be published.
    // Core tools are distribution-trusted and skip signature verification, so allowing
    // them on the registry would let anyone publish unsigned tools with a "core" badge.
    if matches!(
        manifest.manifest.trust_tier,
        agentos_types::TrustTier::Core | agentos_types::TrustTier::Blocked
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Cannot publish tools with 'core' or 'blocked' trust tier".into(),
            }),
        )
            .into_response();
    }

    // Validate tool name — prevent path traversal via malicious names.
    if let Err(e) = validate_tool_name(&manifest.manifest.name) {
        return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })).into_response();
    }

    // Verify the Ed25519 signature (reuse agentos-tools signing logic).
    if let Err(e) = agentos_tools::signing::verify_manifest(&manifest) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Signature verification failed: {}", e),
            }),
        )
            .into_response();
    }

    let now = chrono::Utc::now().to_rfc3339();
    let entry = crate::models::ToolEntry {
        name: manifest.manifest.name.clone(),
        version: manifest.manifest.version.clone(),
        description: manifest.manifest.description.clone(),
        author: manifest.manifest.author.clone(),
        author_pubkey: manifest.manifest.author_pubkey.clone().unwrap_or_default(),
        signature: manifest.manifest.signature.clone().unwrap_or_default(),
        tags: manifest.manifest.tags.clone().unwrap_or_default(),
        manifest_toml: req.manifest_toml.clone(),
        downloads: 0,
        created_at: now.clone(),
        updated_at: now,
    };

    match db.insert_tool(&entry) {
        Ok(()) => {
            tracing::info!(
                name = %entry.name,
                version = %entry.version,
                "Tool published"
            );
            (
                StatusCode::CREATED,
                Json(PublishResponse {
                    name: entry.name,
                    version: entry.version,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("Failed to publish: {}", e),
            }),
        )
            .into_response(),
    }
}

/// Validate a tool name to prevent path traversal and filesystem issues.
fn validate_tool_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Tool name cannot be empty".into());
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(format!("Tool name contains invalid characters: '{}'", name));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "Tool name must contain only alphanumeric, '-', or '_': '{}'",
            name
        ));
    }
    Ok(())
}
