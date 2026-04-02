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
    let agent_list = match state.service.list_agents().await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Failed to list agents: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to list agents").into_response();
        }
    };
    let agents: Vec<_> = agent_list
        .iter()
        .map(|a| {
            context! {
                id => a.id.to_string(),
                name => a.name.clone(),
                provider => a.provider.clone(),
                model => a.model.clone(),
                status => a.status.clone(),
                description => Option::<String>::None,
                roles => a.roles.clone(),
                current_task => Option::<String>::None,
                created_at => a.connected_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                last_active => a.connected_at.format("%Y-%m-%d %H:%M:%S").to_string(),
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
        breadcrumbs => vec![context! { label => "Agents" }],
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
    use agentos_api::types::ConnectAgentRequest;

    let req = ConnectAgentRequest {
        name: form.name.clone(),
        provider: form.provider.clone(),
        model: form.model.clone(),
        base_url: None,
        roles: vec![],
    };

    match state.service.connect_agent(req).await {
        Ok(_) => {
            let mut response = axum::response::Redirect::to("/agents").into_response();
            let trigger = serde_json::json!({
                "showToast": {"message": format!("Agent '{}' connected", form.name), "type": "success"}
            })
            .to_string();
            if let Ok(hv) = axum::http::HeaderValue::from_str(&trigger) {
                response.headers_mut().insert("HX-Trigger", hv);
            }
            response
        }
        Err(e) => {
            tracing::error!(agent = %form.name, error = %e, "Failed to connect agent");
            let mut response = (StatusCode::BAD_REQUEST, "Failed to connect agent").into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Failed to connect agent","type":"error"}}"#,
                ),
            );
            response
        }
    }
}

pub async fn disconnect(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    // Look up agent ID via the service agent list.
    let agent_id = match state.service.list_agents().await {
        Ok(agents) => agents.into_iter().find(|a| a.name == name).map(|a| a.id),
        Err(e) => {
            tracing::error!(agent = %name, error = %e, "Failed to look up agent for disconnect");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    match agent_id {
        Some(id) => match state.service.disconnect_agent(id).await {
            Ok(()) => {
                let mut response = StatusCode::NO_CONTENT.into_response();
                response.headers_mut().insert(
                    "HX-Trigger",
                    axum::http::HeaderValue::from_static(
                        r#"{"showToast":{"message":"Agent disconnected","type":"success"}}"#,
                    ),
                );
                response
            }
            Err(agentos_api::ApiError::NotFound(_)) => StatusCode::NOT_FOUND.into_response(),
            Err(e) => {
                tracing::error!(agent = %name, error = %e, "Failed to disconnect agent");
                let mut response = StatusCode::INTERNAL_SERVER_ERROR.into_response();
                response.headers_mut().insert(
                    "HX-Trigger",
                    axum::http::HeaderValue::from_static(
                        r#"{"showToast":{"message":"Failed to disconnect agent","type":"error"}}"#,
                    ),
                );
                response
            }
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
