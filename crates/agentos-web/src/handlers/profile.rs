use crate::state::AppState;
use axum::extract::{Form, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ForgetForm {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct EditForm {
    pub id: String,
    pub value: Option<String>,
    pub confidence: Option<f32>,
    pub category: Option<String>,
}

/// GET /profile — list stored user-profile entries.
pub async fn list(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(100);
    let entries = state
        .kernel
        .user_profile_store
        .list(limit)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            context! {
                id => e.id.to_string(),
                category => e.category.as_str(),
                key => e.key,
                value => e.value,
                confidence => e.confidence,
                pin_rank => e.pin_rank,
                usage_count => e.usage_count,
                status => e.status.as_str(),
                created_at => e.created_at.to_rfc3339(),
                updated_at => e.updated_at.to_rfc3339(),
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "User Profile",
        breadcrumbs => vec![
            context! { label => "System", href => "/management" },
            context! { label => "User Profile" },
        ],
        entries,
        csrf_token,
    };
    super::render(&state.templates, "profile.html", ctx)
}

/// POST /profile/forget — remove a profile entry.
pub async fn forget(State(state): State<AppState>, Form(form): Form<ForgetForm>) -> Response {
    let _ = state.kernel.user_profile_store.forget(&form.id).await;
    Redirect::to("/profile").into_response()
}

/// POST /profile/edit — edit a profile entry.
pub async fn edit(State(state): State<AppState>, Form(form): Form<EditForm>) -> Response {
    let patch = agentos_types::ProfilePatch {
        category: form
            .category
            .as_deref()
            .map(agentos_types::ProfileCategory::from_str_lossy),
        value: form.value,
        confidence: form.confidence,
        pin_rank: None,
        status: None,
    };
    let _ = state.kernel.user_profile_store.edit(&form.id, patch).await;
    Redirect::to("/profile").into_response()
}
