use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    pub status: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl ApiError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn error_code(&self) -> &str {
        match self {
            Self::NotFound(_) => "NOT_FOUND",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden(_) => "FORBIDDEN",
            Self::BadRequest(_) => "BAD_REQUEST",
            Self::Conflict(_) => "CONFLICT",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ApiErrorBody {
            code: self.error_code().to_string(),
            message: self.to_string(),
            status: status.as_u16(),
        };
        (status, Json(serde_json::json!({ "error": body }))).into_response()
    }
}

impl From<agentos_types::AgentOSError> for ApiError {
    fn from(err: agentos_types::AgentOSError) -> Self {
        match &err {
            agentos_types::AgentOSError::TaskNotFound(_) => Self::NotFound(err.to_string()),
            agentos_types::AgentOSError::AgentNotFound(_) => Self::NotFound(err.to_string()),
            agentos_types::AgentOSError::ToolNotFound(_) => Self::NotFound(err.to_string()),
            agentos_types::AgentOSError::SecretNotFound(_) => Self::NotFound(err.to_string()),
            agentos_types::AgentOSError::PermissionDenied { .. } => {
                Self::Forbidden(err.to_string())
            }
            agentos_types::AgentOSError::InvalidToken { .. } => Self::Unauthorized,
            agentos_types::AgentOSError::TokenExpired => Self::Unauthorized,
            agentos_types::AgentOSError::ToolBlocked { .. } => Self::Forbidden(err.to_string()),
            agentos_types::AgentOSError::SchemaValidation(_) => Self::BadRequest(err.to_string()),
            agentos_types::AgentOSError::RateLimited { .. } => Self::Conflict(err.to_string()),
            _ => Self::Internal(err.to_string()),
        }
    }
}
