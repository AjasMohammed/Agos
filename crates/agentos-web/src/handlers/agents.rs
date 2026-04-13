use crate::state::AppState;
use agentos_types::ThinkingLevel;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
}

/// Build an HTMX-friendly error response that targets the `#connect-form-error` element
/// inside the modal, keeping the modal open so the user sees the validation message.
fn form_error(status: StatusCode, message: &str) -> Response {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let html = format!(
        r#"<small class="form-error" id="connect-form-error" role="alert">{escaped}</small>"#,
    );
    let mut response = (status, Html(html)).into_response();
    response.headers_mut().insert(
        "HX-Retarget",
        HeaderValue::from_static("#connect-form-error"),
    );
    response
        .headers_mut()
        .insert("HX-Reswap", HeaderValue::from_static("outerHTML"));
    let trigger = serde_json::json!({
        "showToast": {"message": message, "type": "error"}
    })
    .to_string();
    if let Ok(hv) = HeaderValue::from_str(&trigger) {
        response.headers_mut().insert("HX-Trigger", hv);
    }
    response
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

    // Build provider list from the kernel's ProviderCatalog.
    let providers: Vec<_> = {
        let catalog = state.kernel.provider_catalog.read().unwrap();
        let mut list: Vec<_> = catalog
            .list()
            .into_iter()
            .map(|p| {
                context! {
                    id => p.name.clone(),
                    label => format!("{} ({})", p.display_name, p.name),
                }
            })
            .collect();
        // Ensure the four built-in providers are always present.
        let names: std::collections::HashSet<String> = list
            .iter()
            .filter_map(|p| {
                p.get_attr("id")
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
            })
            .collect();
        for (id, label) in [
            ("ollama", "Ollama"),
            ("openai", "OpenAI"),
            ("anthropic", "Anthropic"),
            ("gemini", "Gemini"),
        ] {
            if !names.contains(id) {
                list.push(context! { id => id, label => label });
            }
        }
        list
    };

    let ctx = context! {
        page_title => "Agents",
        breadcrumbs => vec![context! { label => "Agents" }],
        agents,
        providers,
        csrf_token,
    };
    super::render(&state.templates, "agents.html", ctx)
}

#[derive(Deserialize)]
pub struct ConnectForm {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub roles: Option<String>,
    pub description: Option<String>,
    pub thinking_level: Option<String>,
    pub system_prompt: Option<String>,
}

fn parse_thinking_level(value: &str) -> Option<ThinkingLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Some(ThinkingLevel::Off),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

pub async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<ConnectForm>,
) -> Response {
    use agentos_api::types::ConnectAgentRequest;

    let roles = form
        .roles
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    let req = ConnectAgentRequest {
        name: form.name.clone(),
        provider: form.provider.clone(),
        model: form.model.clone(),
        base_url: form.base_url.clone().filter(|s| !s.trim().is_empty()),
        roles,
        description: form.description.clone().filter(|s| !s.trim().is_empty()),
        thinking_level: match form
            .thinking_level
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(level) => match parse_thinking_level(level) {
                Some(v) => Some(v),
                None => return (StatusCode::BAD_REQUEST, "Invalid thinking level").into_response(),
            },
            None => None,
        },
        system_prompt: match form
            .system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(v) if v.len() > 16_384 => {
                return (
                    StatusCode::BAD_REQUEST,
                    "System prompt too long (max 16,384 chars)",
                )
                    .into_response();
            }
            Some(v) => Some(v.to_string()),
            None => None,
        },
    };

    // Server-side validation.
    if form.name.trim().is_empty() {
        return form_error(StatusCode::UNPROCESSABLE_ENTITY, "Agent name is required");
    }
    if !form
        .name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return form_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Agent name must be lowercase alphanumeric with dashes",
        );
    }
    if form.model.trim().is_empty() {
        return form_error(StatusCode::UNPROCESSABLE_ENTITY, "Model is required");
    }
    if let Some(ref url_str) = form.base_url {
        if !url_str.trim().is_empty() && reqwest::Url::parse(url_str).is_err() {
            return form_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Base URL must be a valid URL",
            );
        }
    }

    match state.service.connect_agent(req).await {
        Ok(_) => {
            let is_htmx = headers
                .get("HX-Request")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);

            let mut response = if is_htmx {
                let agent_list = match state.service.list_agents().await {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!("Failed to list agents after connect: {e}");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Agent connected but failed to refresh list",
                        )
                            .into_response();
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
                let ctx = context! { agents };
                super::render(&state.templates, "partials/agent_card.html", ctx)
            } else {
                axum::response::Redirect::to("/agents").into_response()
            };

            let trigger = serde_json::json!({
                "showToast": {"message": format!("Agent '{}' connected", form.name), "type": "success"},
                "closeAgentModal": true,
                "agent-connected": {"name": form.name}
            })
            .to_string();
            if let Ok(hv) = HeaderValue::from_str(&trigger) {
                response.headers_mut().insert("HX-Trigger", hv);
            }
            response
        }
        Err(e) => {
            tracing::error!(agent = %form.name, error = %e, "Failed to connect agent");
            form_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!("Failed to connect agent: {e}"),
            )
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
