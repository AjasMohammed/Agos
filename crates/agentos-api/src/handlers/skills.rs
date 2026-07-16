//! Read-only skills library: list installed skills, get one by name.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiSkillDetail, ApiSkillSummary};

/// `GET /api/v1/skills` — List installed skills.
#[utoipa::path(
    get,
    path = "/api/v1/skills",
    tag = "skills",
    operation_id = "skills_list",
    responses(
        (status = 200, description = "Installed skills", body = crate::response::Envelope<Vec<crate::types::ApiSkillSummary>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiSkillSummary>>>, ApiError> {
    require_permission(&key, "skills:r")?;
    Ok(Json(Envelope::new(svc.list_skills().await?)))
}

/// `GET /api/v1/skills/{name}` — Get one skill's full detail.
#[utoipa::path(
    get,
    path = "/api/v1/skills/{name}",
    tag = "skills",
    operation_id = "skills_get",
    params(("name" = String, Path, description = "Skill name")),
    responses(
        (status = 200, description = "Skill detail", body = crate::response::Envelope<crate::types::ApiSkillDetail>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Skill not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<ApiSkillDetail>>, ApiError> {
    require_permission(&key, "skills:r")?;
    Ok(Json(Envelope::new(svc.get_skill(name).await?)))
}
