use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use minijinja::context;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let secrets = state.kernel.vault.list().unwrap_or_default();

    let secret_rows: Vec<_> = secrets
        .iter()
        .map(|s| {
            context! {
                name => s.name.clone(),
                scope => format!("{:?}", s.scope),
                created_at => s.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                last_used_at => s.last_used_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            }
        })
        .collect();

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { secrets => secret_rows };
        return super::render(&state.templates, "partials/tool_card.html", ctx);
    }

    let ctx = context! {
        page_title => "Secrets",
        secrets => secret_rows,
    };
    super::render(&state.templates, "secrets.html", ctx)
}

#[derive(Deserialize)]
pub struct CreateForm {
    pub name: String,
    pub value: String,
    pub scope: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<CreateForm>,
) -> Response {
    use agentos_types::{SecretOwner, SecretScope};

    let scope = match form.scope.as_deref() {
        Some("global") | None => SecretScope::Global,
        _ => SecretScope::Global,
    };

    match state
        .kernel
        .vault
        .set(&form.name, &form.value, SecretOwner::Kernel, scope)
    {
        Ok(_) => axum::response::Redirect::to("/secrets").into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create secret: {}", e),
        )
            .into_response(),
    }
}

pub async fn revoke(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match state.kernel.vault.revoke(&name) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}
