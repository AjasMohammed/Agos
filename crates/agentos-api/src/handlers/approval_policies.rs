//! Approval-policy (standing-grant) endpoints: list, add, revoke.
//!
//! These expose the kernel's `ApprovalPolicyStore` — persisted "allow always"
//! overrides that lift `Prompt → Allow` in the approval hook — over REST so the
//! panel can surface a standing-grants registry and an "approve & remember"
//! action on the escalation queue.

use axum::extract::{Path, State};
use axum::Extension;
use axum::Json;
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::Envelope;
use crate::service::KernelService;
use crate::types::{AddApprovalPolicyRequest, ApiApprovalPolicy};

/// `GET /api/v1/approval-policies` — List active standing grants.
#[utoipa::path(
    get,
    path = "/api/v1/approval-policies",
    tag = "approval-policies",
    operation_id = "approval_policies_list",
    responses(
        (status = 200, description = "Active approval policies", body = crate::response::Envelope<Vec<crate::types::ApiApprovalPolicy>>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
) -> Result<Json<Envelope<Vec<ApiApprovalPolicy>>>, ApiError> {
    require_permission(&key, "approvals:r")?;
    let policies = svc.list_approval_policies().await?;
    Ok(Json(Envelope::new(policies)))
}

/// `POST /api/v1/approval-policies` — Add a standing grant.
///
/// Returns 409 Conflict if an active policy already exists for the same
/// (tool_name, path_glob, agent_id) scope.
#[utoipa::path(
    post,
    path = "/api/v1/approval-policies",
    tag = "approval-policies",
    operation_id = "approval_policies_add",
    request_body = AddApprovalPolicyRequest,
    responses(
        (status = 200, description = "Policy created", body = crate::response::Envelope<crate::types::ApiApprovalPolicy>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 409, description = "Duplicate policy for this scope", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn add(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Json(req): Json<AddApprovalPolicyRequest>,
) -> Result<Json<Envelope<ApiApprovalPolicy>>, ApiError> {
    require_permission(&key, "approvals:w")?;
    let policy = svc.add_approval_policy(req).await?;
    Ok(Json(Envelope::new(policy)))
}

/// `DELETE /api/v1/approval-policies/{id}` — Revoke a standing grant.
#[utoipa::path(
    delete,
    path = "/api/v1/approval-policies/{id}",
    tag = "approval-policies",
    operation_id = "approval_policies_revoke",
    params(("id" = i64, Path, description = "Approval policy ID")),
    responses(
        (status = 200, description = "Policy revoked", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "Policy not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<i64>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "approvals:w")?;
    svc.revoke_approval_policy(id).await?;
    Ok(Json(Envelope::new(
        serde_json::json!({ "revoked": true, "id": id }),
    )))
}
