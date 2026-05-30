use agentos_types::NotificationID;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NotificationFilter {
    pub unread_only: Option<bool>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NotificationResponseRequest {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NotificationSummary {
    #[schema(value_type = String)]
    pub id: NotificationID,
    pub subject: String,
    pub priority: String,
    pub read: bool,
    pub timestamp: String,
    /// Source label (e.g. "Kernel", agent name).
    #[serde(default)]
    pub from: String,
    /// Truncated body text for list views.
    #[serde(default)]
    pub body: String,
}
