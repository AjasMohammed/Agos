use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

/// GET /agents/{name}/detail — agent detail page with permissions, tasks, and cost.
pub async fn detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
    jar: CookieJar,
) -> Response {
    let registry = state.kernel.agent_registry.read().await;
    let agent = match registry.get_by_name(&name) {
        Some(a) => a.clone(),
        None => {
            drop(registry);
            return (StatusCode::NOT_FOUND, "Agent not found").into_response();
        }
    };
    drop(registry);

    // Permissions.
    let permissions: Vec<_> = agent
        .permissions
        .entries
        .iter()
        .map(|p| {
            context! {
                resource => p.resource.clone(),
                read => p.read,
                write => p.write,
                execute => p.execute,
                query => p.query,
                observe => p.observe,
            }
        })
        .collect();

    let deny_entries = agent.permissions.deny_entries.clone();

    // Active tasks for this agent.
    let all_tasks = state.kernel.scheduler.list_tasks().await;
    let agent_tasks: Vec<_> = all_tasks
        .iter()
        .filter(|t| t.agent_id == agent.id)
        .map(|t| {
            context! {
                id => t.id.to_string(),
                state => format!("{:?}", t.state),
                prompt_preview => t.prompt_preview.clone(),
                created_at => t.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                tool_calls => t.tool_calls,
                tokens_used => t.tokens_used,
            }
        })
        .collect();

    let task_count = agent_tasks.len();

    // Cost snapshot for this agent.
    let cost_snap = state.kernel.cost_tracker.get_snapshot(&agent.id).await;
    let cost = cost_snap.as_ref().map(|s| {
        context! {
            cost_usd => format!("{:.6}", s.cost_usd),
            tokens_used => s.tokens_used,
            tool_calls => s.tool_calls,
            cost_pct => format!("{:.1}", s.cost_pct),
            tokens_pct => format!("{:.1}", s.tokens_pct),
            max_cost_usd_per_day => format!("{:.2}", s.budget.max_cost_usd_per_day),
            max_tokens_per_day => s.budget.max_tokens_per_day,
            has_cost_budget => s.budget.max_cost_usd_per_day > 0.0,
            has_token_budget => s.budget.max_tokens_per_day > 0,
        }
    });

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let short_id = agent.id.to_string().chars().take(8).collect::<String>();

    let ctx = context! {
        page_title => format!("Agent: {}", agent.name),
        breadcrumbs => vec![
            context! { label => "Agents", href => "/agents" },
            context! { label => agent.name.clone() },
        ],
        agent_id => agent.id.to_string(),
        short_id,
        name => agent.name.clone(),
        provider => format!("{:?}", agent.provider),
        model => agent.model.clone(),
        status => format!("{:?}", agent.status),
        description => agent.description.clone(),
        roles => agent.roles.clone(),
        created_at => agent.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        last_active => agent.last_active.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        public_key_hex => agent.public_key_hex.clone(),
        permissions,
        deny_entries,
        tasks => agent_tasks,
        task_count,
        cost,
        csrf_token,
    };
    super::render(&state.templates, "agents/detail.html", ctx)
}

/// POST /agents/{name}/permissions — grant a permission to an agent.
pub async fn grant_permission(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::Form(form): axum::Form<GrantPermissionForm>,
) -> Response {
    match state
        .kernel
        .api_grant_permission(name.clone(), form.permission.clone())
        .await
    {
        Ok(()) => {
            let redirect_url = format!("/agents/{}/detail", name);
            let mut response = axum::response::Redirect::to(&redirect_url).into_response();
            let trigger = serde_json::json!({
                "showToast": {"message": format!("Permission '{}' granted", form.permission), "type": "success"}
            })
            .to_string();
            if let Ok(hv) = axum::http::HeaderValue::from_str(&trigger) {
                response.headers_mut().insert("HX-Trigger", hv);
            }
            response
        }
        Err(msg) => {
            tracing::error!(agent = %name, error = %msg, "Failed to grant permission");
            let mut response =
                (StatusCode::BAD_REQUEST, "Failed to grant permission").into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Failed to grant permission","type":"error"}}"#,
                ),
            );
            response
        }
    }
}

#[derive(Deserialize)]
pub struct GrantPermissionForm {
    pub permission: String,
}

/// POST /agents/{name}/permissions/revoke — revoke a permission from an agent.
pub async fn revoke_permission(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::Form(form): axum::Form<RevokePermissionForm>,
) -> Response {
    match state
        .kernel
        .api_revoke_permission(name.clone(), form.permission.clone())
        .await
    {
        Ok(()) => {
            let redirect_url = format!("/agents/{}/detail", name);
            let mut response = axum::response::Redirect::to(&redirect_url).into_response();
            let trigger = serde_json::json!({
                "showToast": {"message": format!("Permission '{}' revoked", form.permission), "type": "info"}
            })
            .to_string();
            if let Ok(hv) = axum::http::HeaderValue::from_str(&trigger) {
                response.headers_mut().insert("HX-Trigger", hv);
            }
            response
        }
        Err(msg) => {
            tracing::error!(agent = %name, error = %msg, "Failed to revoke permission");
            let mut response =
                (StatusCode::BAD_REQUEST, "Failed to revoke permission").into_response();
            response.headers_mut().insert(
                "HX-Trigger",
                axum::http::HeaderValue::from_static(
                    r#"{"showToast":{"message":"Failed to revoke permission","type":"error"}}"#,
                ),
            );
            response
        }
    }
}

#[derive(Deserialize)]
pub struct RevokePermissionForm {
    pub permission: String,
}
