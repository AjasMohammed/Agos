use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
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

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Agents",
        agents,
        csrf_token,
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
    use agentos_types::LLMProvider;

    let provider = match form.provider.to_lowercase().as_str() {
        "ollama" => LLMProvider::Ollama,
        "openai" => LLMProvider::OpenAI,
        "anthropic" => LLMProvider::Anthropic,
        "gemini" => LLMProvider::Gemini,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "Unknown provider. Must be one of: ollama, openai, anthropic, gemini",
            )
                .into_response();
        }
    };

    match state
        .kernel
        .api_connect_agent(form.name.clone(), provider, form.model, None, vec![])
        .await
    {
        Ok(()) => axum::response::Redirect::to("/agents").into_response(),
        Err(msg) => {
            tracing::error!(agent = %form.name, error = %msg, "Failed to connect agent");
            (StatusCode::BAD_REQUEST, "Failed to connect agent").into_response()
        }
    }
}

pub async fn disconnect(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let agent_id = {
        let registry = state.kernel.agent_registry.read().await;
        registry.get_by_name(&name).map(|a| a.id)
    };

    match agent_id {
        Some(id) => match state.kernel.api_disconnect_agent(id).await {
            Ok(()) => StatusCode::NO_CONTENT,
            // Agent may have been disconnected by a concurrent request between the
            // read-lock lookup above and the write-lock acquisition inside the kernel.
            Err(msg) if msg.contains("not found") => StatusCode::NOT_FOUND,
            Err(msg) => {
                tracing::error!(agent = %name, error = %msg, "Failed to disconnect agent");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        },
        None => StatusCode::NOT_FOUND,
    }
}
