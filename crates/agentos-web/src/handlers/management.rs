use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;

fn plugin_status_label(status: &agentos_kernel::plugin_registry::PluginStatus) -> &'static str {
    match status {
        agentos_kernel::plugin_registry::PluginStatus::Discovered => "discovered",
        agentos_kernel::plugin_registry::PluginStatus::Active => "active",
        agentos_kernel::plugin_registry::PluginStatus::Disabled => "disabled",
        agentos_kernel::plugin_registry::PluginStatus::Blocked { .. } => "blocked",
    }
}

pub async fn page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let plugins = state
        .kernel
        .plugin_registry
        .list()
        .await
        .into_iter()
        .map(|p| {
            context! {
                id => p.manifest.id.clone(),
                version => p.manifest.version.clone(),
                description => p.manifest.description.clone(),
                trust_tier => format!("{:?}", p.manifest.trust_tier).to_lowercase(),
                status => plugin_status_label(&p.status),
                blocked_reason => match p.status {
                    agentos_kernel::plugin_registry::PluginStatus::Blocked { reason } => Some(reason),
                    _ => None,
                },
            }
        })
        .collect::<Vec<_>>();

    let channels = match state.kernel.channel_registry.list_active().await {
        Ok(rows) => rows
            .into_iter()
            .map(|ch| {
                context! {
                    id => ch.id.to_string(),
                    kind => ch.kind.to_string(),
                    display_name => ch.display_name,
                    external_id => ch.external_id,
                    connected_at => ch.connected_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    last_active => ch.last_active.format("%Y-%m-%d %H:%M:%S").to_string(),
                }
            })
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list channels for management page");
            Vec::new()
        }
    };

    let schedules = state
        .kernel
        .schedule_manager
        .list_jobs()
        .await
        .into_iter()
        .map(|s| {
            context! {
                id => s.id.to_string(),
                name => s.name,
                cron_expression => s.cron_expression,
                state => format!("{:?}", s.state),
                agent_name => s.agent_name,
                run_count => s.run_count,
                last_run_at => s.last_run_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default(),
                next_run_at => s.next_run_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    let roles = state
        .kernel
        .profile_manager
        .list_all()
        .into_iter()
        .map(|r| {
            context! {
                name => r.name,
                description => r.description,
                created_at => r.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            }
        })
        .collect::<Vec<_>>();

    let connected_agents = {
        let reg = state.kernel.agent_registry.read().await;
        reg.list_online()
            .into_iter()
            .map(|a| {
                context! {
                    name => a.name,
                    roles => a.roles.join(", "),
                    model => a.model,
                    provider => format!("{:?}", a.provider).to_lowercase(),
                }
            })
            .collect::<Vec<_>>()
    };

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Management",
        breadcrumbs => vec![context! { label => "Management" }],
        plugins,
        channels,
        schedules,
        roles,
        connected_agents,
        config => serde_json::to_value(&state.kernel.config).unwrap_or(serde_json::Value::Null),
        csrf_token,
    };
    super::render(&state.templates, "management.html", ctx)
}

pub async fn plugin_enable(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.kernel.plugin_registry.activate(&id).await {
        Ok(()) => Redirect::to("/management").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to enable plugin '{id}': {e}"),
        )
            .into_response(),
    }
}

pub async fn plugin_disable(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.kernel.plugin_registry.deactivate(&id).await {
        Ok(()) => Redirect::to("/management").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to disable plugin '{id}': {e}"),
        )
            .into_response(),
    }
}

pub async fn schedule_pause(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let parsed: agentos_types::ScheduleID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid schedule ID").into_response(),
    };
    match state.kernel.schedule_manager.pause(&parsed).await {
        Ok(()) => Redirect::to("/management").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to pause schedule '{id}': {e}"),
        )
            .into_response(),
    }
}

pub async fn schedule_resume(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let parsed: agentos_types::ScheduleID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid schedule ID").into_response(),
    };
    match state.kernel.schedule_manager.resume(&parsed).await {
        Ok(()) => Redirect::to("/management").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to resume schedule '{id}': {e}"),
        )
            .into_response(),
    }
}

pub async fn schedule_delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let parsed: agentos_types::ScheduleID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid schedule ID").into_response(),
    };
    match state.kernel.schedule_manager.delete(&parsed).await {
        Ok(()) => Redirect::to("/management").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to delete schedule '{id}': {e}"),
        )
            .into_response(),
    }
}
