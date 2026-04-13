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

pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Response {
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
                channels => p.manifest.channels,
                tools => p.manifest.tools,
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Plugins",
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Plugins" },
        ],
        plugins,
        csrf_token,
    };
    super::render(&state.templates, "plugins.html", ctx)
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Response {
    let plugin = state
        .kernel
        .plugin_registry
        .list()
        .await
        .into_iter()
        .find(|p| p.manifest.id == id);

    let Some(plugin) = plugin else {
        return (StatusCode::NOT_FOUND, format!("Plugin '{}' not found", id)).into_response();
    };

    let status = plugin_status_label(&plugin.status);
    let blocked_reason = match plugin.status {
        agentos_kernel::plugin_registry::PluginStatus::Blocked { reason } => Some(reason),
        _ => None,
    };

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => format!("Plugin {}", plugin.manifest.id),
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Plugins", href => "/plugins" },
            context! { label => plugin.manifest.id.clone() },
        ],
        plugin => context! {
            id => plugin.manifest.id.clone(),
            display_name => plugin.manifest.display_name.clone(),
            version => plugin.manifest.version.clone(),
            description => plugin.manifest.description.clone(),
            trust_tier => format!("{:?}", plugin.manifest.trust_tier).to_lowercase(),
            status,
            blocked_reason,
            channels => plugin.manifest.channels.iter().map(|c| context! {
                id => c.id.clone(),
                display_name => c.display_name.clone(),
                capabilities => c.capabilities.clone(),
            }).collect::<Vec<_>>(),
            tools => plugin.manifest.tools.clone(),
            permissions => plugin.manifest.permissions.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        },
        csrf_token,
    };
    super::render(&state.templates, "plugin_detail.html", ctx)
}

pub async fn discover(State(state): State<AppState>) -> Response {
    let data_dir = std::path::PathBuf::from(&state.kernel.config.tools.data_dir);
    let base = data_dir.parent().unwrap_or(&data_dir);
    let plugin_dirs = vec![base.join("plugins/core"), base.join("plugins/user")];
    let _ = state.kernel.plugin_registry.discover(&plugin_dirs).await;
    Redirect::to("/plugins").into_response()
}

pub async fn enable(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.kernel.plugin_registry.activate(&id).await {
        Ok(()) => Redirect::to("/plugins").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to enable plugin '{id}': {e}"),
        )
            .into_response(),
    }
}

pub async fn disable(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.kernel.plugin_registry.deactivate(&id).await {
        Ok(()) => Redirect::to("/plugins").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to disable plugin '{id}': {e}"),
        )
            .into_response(),
    }
}
