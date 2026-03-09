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
    let registry = state.kernel.agent_registry.read().await;
    let agents: Vec<_> = registry
        .list_all()
        .iter()
        .map(|a| {
            context! {
                id => a.id.to_string(),
                name => a.name.clone(),
                provider => format!("{:?}", a.provider),
                model => a.model.clone(),
                status => format!("{:?}", a.status),
                description => a.description.clone(),
                roles => a.roles.clone(),
                current_task => a.current_task.as_ref().map(|t| t.to_string()),
                created_at => a.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                last_active => a.last_active.format("%Y-%m-%d %H:%M:%S").to_string(),
            }
        })
        .collect();

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { agents };
        return super::render(&state.templates, "partials/agent_card.html", ctx);
    }

    let ctx = context! {
        page_title => "Agents",
        agents,
    };
    super::render(&state.templates, "agents.html", ctx)
}

#[derive(Deserialize)]
pub struct ConnectForm {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub description: Option<String>,
}

pub async fn connect(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<ConnectForm>,
) -> Response {
    use agentos_types::*;

    let provider = match form.provider.to_lowercase().as_str() {
        "ollama" => LLMProvider::Ollama,
        "openai" => LLMProvider::OpenAI,
        "anthropic" => LLMProvider::Anthropic,
        "gemini" => LLMProvider::Gemini,
        other => LLMProvider::Custom(other.to_string()),
    };

    let profile = AgentProfile {
        id: AgentID::new(),
        name: form.name,
        provider,
        model: form.model,
        status: AgentStatus::Idle,
        permissions: PermissionSet::new(),
        roles: vec![],
        current_task: None,
        description: form.description.unwrap_or_default(),
        created_at: chrono::Utc::now(),
        last_active: chrono::Utc::now(),
    };

    state.kernel.agent_registry.write().await.register(profile);

    axum::response::Redirect::to("/agents").into_response()
}

pub async fn disconnect(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let mut registry = state.kernel.agent_registry.write().await;
    if let Some(agent) = registry.get_by_name(&name) {
        let id = agent.id.clone();
        registry.remove(&id);
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}
