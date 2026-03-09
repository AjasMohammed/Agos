use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use minijinja::context;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
    pub filter_type: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let registry = state.kernel.tool_registry.read().await;
    let all_tools = registry.list_all();

    let tools: Vec<_> = all_tools
        .iter()
        .filter(|t| {
            if let Some(ref ft) = query.filter_type {
                let exec_type = format!("{:?}", t.manifest.executor.executor_type);
                exec_type.to_lowercase().contains(&ft.to_lowercase())
            } else {
                true
            }
        })
        .map(|t| {
            context! {
                id => t.id.to_string(),
                name => t.manifest.manifest.name.clone(),
                description => t.manifest.manifest.description.clone(),
                version => t.manifest.manifest.version.clone(),
                executor_type => format!("{:?}", t.manifest.executor.executor_type),
                status => format!("{:?}", t.status),
                network => t.manifest.sandbox.network,
                fs_write => t.manifest.sandbox.fs_write,
            }
        })
        .collect();

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { tools };
        return super::render(&state.templates, "partials/tool_card.html", ctx);
    }

    let ctx = context! {
        page_title => "Tools",
        tools,
        tool_count => tools.len(),
    };
    super::render(&state.templates, "tools.html", ctx)
}

#[derive(Deserialize)]
pub struct InstallForm {
    pub manifest_path: String,
}

pub async fn install(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<InstallForm>,
) -> Response {
    let path = std::path::Path::new(&form.manifest_path);
    if !path.exists() {
        return (StatusCode::BAD_REQUEST, "Manifest file not found").into_response();
    }

    match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<agentos_types::ToolManifest>(&content) {
            Ok(manifest) => {
                state
                    .kernel
                    .tool_registry
                    .write()
                    .await
                    .register(manifest);
                axum::response::Redirect::to("/tools").into_response()
            }
            Err(e) => (
                StatusCode::BAD_REQUEST,
                format!("Invalid manifest: {}", e),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to read file: {}", e),
        )
            .into_response(),
    }
}

pub async fn remove(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let mut registry = state.kernel.tool_registry.write().await;
    match registry.remove(&name) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}
