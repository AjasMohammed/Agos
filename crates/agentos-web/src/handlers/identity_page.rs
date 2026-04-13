use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn page(
    State(state): State<AppState>,
    Path(name): Path<String>,
    jar: CookieJar,
) -> Response {
    let agent = {
        let registry = state.kernel.agent_registry.read().await;
        registry.get_by_name(&name).cloned()
    };

    let Some(agent) = agent else {
        return (StatusCode::NOT_FOUND, "Agent not found").into_response();
    };

    let public_key = agent.public_key_hex.clone().unwrap_or_default();
    let fingerprint = public_key.chars().take(16).collect::<String>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => format!("Identity: {}", name),
        breadcrumbs => vec![
            context! { label => "Agents", href => "/agents" },
            context! { label => name.clone(), href => format!("/agents/{}/detail", name) },
            context! { label => "Identity" },
        ],
        name => agent.name,
        agent_id => agent.id.to_string(),
        public_key,
        fingerprint,
        created_at => agent.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        last_active => agent.last_active.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        status => format!("{:?}", agent.status),
        csrf_token,
    };
    super::render(&state.templates, "identity.html", ctx)
}
