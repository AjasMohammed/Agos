//! Role-management endpoints: list, create, get, delete.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{ApiRole, CreateRoleRequest};

/// `GET /api/v1/roles` — List all roles.
#[utoipa::path(
    get,
    path = "/api/v1/roles",
    tag = "roles",
    operation_id = "roles_list",
    responses(
        (status = 200, description = "List of roles", body = crate::response::Envelope<Vec<crate::types::ApiRole>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiRole>>>, ApiError> {
    require_permission(&key, "roles:r")?;
    let roles = svc.list_roles().await?;
    Ok(Json(Envelope::new(roles)))
}

/// `POST /api/v1/roles` — Create a new role with permissions.
///
/// Returns 409 Conflict if a role with the same name already exists.
#[utoipa::path(
    post,
    path = "/api/v1/roles",
    tag = "roles",
    operation_id = "roles_create",
    request_body = CreateRoleRequest,
    responses(
        (status = 200, description = "Role created", body = crate::response::Envelope<crate::types::ApiRole>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 409, description = "Role already exists", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<CreateRoleRequest>,
) -> Result<Json<Envelope<ApiRole>>, ApiError> {
    require_permission(&key, "roles:w")?;
    let role = svc.create_role(req).await?;
    Ok(Json(Envelope::new(role)))
}

/// `GET /api/v1/roles/{name}` — Get a single role by name.
#[utoipa::path(
    get,
    path = "/api/v1/roles/{name}",
    tag = "roles",
    operation_id = "roles_get",
    params(("name" = String, Path, description = "Role name")),
    responses(
        (status = 200, description = "Role detail", body = crate::response::Envelope<crate::types::ApiRole>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Role not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<ApiRole>>, ApiError> {
    require_permission(&key, "roles:r")?;
    let role = svc.get_role(&name).await?;
    Ok(Json(Envelope::new(role)))
}

/// `DELETE /api/v1/roles/{name}` — Delete a role by name.
#[utoipa::path(
    delete,
    path = "/api/v1/roles/{name}",
    tag = "roles",
    operation_id = "roles_delete",
    params(("name" = String, Path, description = "Role name")),
    responses(
        (status = 200, description = "Role deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Role not found", body = crate::error::ApiErrorBody),
        (status = 409, description = "Role in use or protected", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(name): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "roles:w")?;
    svc.delete_role(&name).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": name }))))
}
