use crate::state::AppState;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;
use zeroize::Zeroizing;

/// Optional query param used by the list page to surface a just-created endpoint
/// secret in a one-shot banner. The secret is NEVER stored — it only lives in the
/// URL for a single page render after creation.
#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    pub created_id: Option<String>,
    pub created_secret: Option<String>,
    pub rotated_from: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWebhookForm {
    pub agent_name: String,
    pub provider: String,
    pub debounce_seconds: Option<u64>,
}

#[derive(Default)]
struct FlashSecret {
    created_id: String,
    created_secret: Zeroizing<String>,
    rotated_from: String,
}

fn parse_provider(provider: &str) -> Option<agentos_types::WebhookProvider> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "github" => Some(agentos_types::WebhookProvider::GitHub),
        "stripe" => Some(agentos_types::WebhookProvider::Stripe),
        "slack" => Some(agentos_types::WebhookProvider::Slack),
        "pagerduty" => Some(agentos_types::WebhookProvider::PagerDuty),
        "generic" => Some(agentos_types::WebhookProvider::Generic),
        _ => None,
    }
}

async fn render_list(
    State(state): State<AppState>,
    flash: FlashSecret,
    jar: CookieJar,
) -> Response {
    let agents = {
        let reg = state.kernel.agent_registry.read().await;
        reg.list_all()
            .into_iter()
            .map(|a| context! { name => a.name.clone(), id => a.id.to_string() })
            .collect::<Vec<_>>()
    };

    let endpoints = state
        .kernel
        .webhook_registry
        .list_endpoints(None)
        .await
        .into_iter()
        .map(|w| {
            let inbound_url = format!("/api/v1/webhooks/incoming/{}", w.id);
            context! {
                id => w.id.to_string(),
                agent_id => w.agent_id.to_string(),
                provider => w.provider,
                active => w.active,
                debounce_seconds => w.debounce_seconds,
                total_received => w.total_received,
                created_at => w.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                last_received_at => w.last_received_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default(),
                inbound_url,
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    // One-shot banner: pair the just-created ID with its plaintext secret.
    let created_id = flash.created_id;
    // Deref Zeroizing<String> → String for template rendering.
    let created_secret = (*flash.created_secret).clone();
    let rotated_from = flash.rotated_from;
    let created_inbound_url = if created_id.is_empty() {
        String::new()
    } else {
        format!("/api/v1/webhooks/incoming/{}", created_id)
    };
    let ctx = context! {
        page_title => "Webhooks",
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Webhooks" },
        ],
        agents,
        endpoints,
        csrf_token,
        created_id,
        created_secret,
        rotated_from,
        created_inbound_url,
    };
    super::render(&state.templates, "webhooks_page.html", ctx)
}

pub async fn list(
    state: State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
) -> Response {
    if query.created_secret.is_some() {
        tracing::warn!(
            "Ignoring created_secret query param on /webhooks to avoid URL-based secret exposure"
        );
    }
    render_list(
        state,
        FlashSecret {
            created_id: query.created_id.unwrap_or_default(),
            created_secret: Zeroizing::new(String::new()),
            rotated_from: query.rotated_from.unwrap_or_default(),
        },
        jar,
    )
    .await
}

pub async fn create(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CreateWebhookForm>,
) -> Response {
    let provider = match parse_provider(&form.provider) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Invalid provider. Allowed: github, stripe, slack, pagerduty, generic",
            )
                .into_response()
        }
    };

    let agent = {
        let reg = state.kernel.agent_registry.read().await;
        reg.get_by_name(form.agent_name.trim()).cloned()
    };

    let Some(agent) = agent else {
        return (
            StatusCode::BAD_REQUEST,
            format!("Unknown agent '{}'", form.agent_name),
        )
            .into_response();
    };

    let debounce_seconds = form.debounce_seconds.unwrap_or(0);
    match state
        .kernel
        .webhook_registry
        .create_endpoint(agent.id, provider, debounce_seconds)
        .await
    {
        Ok((meta, secret)) => {
            render_list(
                State(state),
                FlashSecret {
                    created_id: meta.id.to_string(),
                    created_secret: Zeroizing::new(secret),
                    rotated_from: String::new(),
                },
                jar,
            )
            .await
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to create webhook endpoint: {e}"),
        )
            .into_response(),
    }
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let parsed: agentos_types::WebhookEndpointID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid endpoint ID").into_response(),
    };

    match state.kernel.webhook_registry.delete_endpoint(&parsed).await {
        Ok(()) => Redirect::to("/webhooks").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to delete webhook endpoint '{}': {e}", id),
        )
            .into_response(),
    }
}

pub async fn rotate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Response {
    let endpoint_id: agentos_types::WebhookEndpointID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid endpoint ID").into_response(),
    };

    let Some(_existing) = state
        .kernel
        .webhook_registry
        .get_endpoint(&endpoint_id)
        .await
    else {
        return (StatusCode::NOT_FOUND, "Webhook endpoint not found").into_response();
    };

    let secret = match state
        .kernel
        .webhook_registry
        .rotate_secret(&endpoint_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Failed to rotate webhook endpoint '{}': {}", id, e),
            )
                .into_response()
        }
    };

    render_list(
        State(state),
        FlashSecret {
            created_id: endpoint_id.to_string(),
            created_secret: Zeroizing::new(secret),
            rotated_from: id,
        },
        jar,
    )
    .await
}
