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
    let secrets = match state.kernel.vault.list().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list secrets");
            vec![]
        }
    };

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
        return super::render(&state.templates, "partials/secret_row.html", ctx);
    }

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Secrets",
        breadcrumbs => vec![context! { label => "Secrets" }],
        secrets => secret_rows,
        csrf_token,
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
    axum::Form(mut form): axum::Form<CreateForm>,
) -> Response {
    use agentos_types::SecretScope;
    use agentos_vault::ZeroizingString;

    // Limit secret value to 64 KiB to prevent memory exhaustion.
    const MAX_SECRET_BYTES: usize = 64 * 1024;
    if form.value.len() > MAX_SECRET_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "Secret value too large (max 64 KiB)",
        )
            .into_response();
    }

    let secret_value = ZeroizingString::new(std::mem::take(&mut form.value));

    let scope = match form.scope.as_deref() {
        Some("global") | None => SecretScope::Global,
        Some("kernel") => SecretScope::Kernel,
        Some(other) => {
            if let Some(agent_id_str) = other.strip_prefix("agent:") {
                match agent_id_str.parse::<agentos_types::AgentID>() {
                    Ok(id) => SecretScope::Agent(id),
                    Err(_) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            "Invalid agent ID in scope: expected 'agent:<uuid>'",
                        )
                            .into_response();
                    }
                }
            } else if let Some(tool_id_str) = other.strip_prefix("tool:") {
                match tool_id_str.parse::<agentos_types::ToolID>() {
                    Ok(id) => SecretScope::Tool(id),
                    Err(_) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            "Invalid tool ID in scope: expected 'tool:<uuid>'",
                        )
                            .into_response();
                    }
                }
            } else {
                return (
                    StatusCode::BAD_REQUEST,
                    "Unrecognized scope. Use 'global', 'kernel', 'agent:<uuid>', or 'tool:<uuid>'.",
                )
                    .into_response();
            }
        }
    };

    // Route through kernel command dispatch for audit logging and scope resolution.
    // secret_value dropped at end of scope -> memory zeroed by ZeroizingString.
    match state
        .kernel
        .api_set_secret(form.name, secret_value.as_str().to_string(), scope)
        .await
    {
        Ok(()) => {
            let mut response = axum::response::Redirect::to("/secrets").into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Secret saved","type":"success"}}"#,
                ),
            );
            response
        }
        Err(msg) => {
            tracing::error!(error = %msg, "Failed to create secret");
            let mut response = (
                StatusCode::BAD_REQUEST,
                format!("Failed to create secret: {}", msg),
            )
                .into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Failed to save secret","type":"error"}}"#,
                ),
            );
            response
        }
    }
}

pub async fn revoke(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    match state.kernel.api_revoke_secret(name.clone()).await {
        Ok(()) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Secret revoked","type":"success"}}"#,
                ),
            );
            response
        }
        Err(msg) if msg.to_lowercase().contains("not found") => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(msg) => {
            tracing::warn!(secret = %name, error = %msg, "Failed to revoke secret");
            let mut response = StatusCode::INTERNAL_SERVER_ERROR.into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Failed to revoke secret","type":"error"}}"#,
                ),
            );
            response
        }
    }
}
