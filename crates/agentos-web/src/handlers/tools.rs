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
    pub filter_type: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
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

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let tool_count = tools.len();
    let ctx = context! {
        page_title => "Tools",
        tools,
        tool_count,
        csrf_token,
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
    let manifest_path_str = form.manifest_path.clone();
    let requested_path = std::path::PathBuf::from(&manifest_path_str);

    // Step 1: Canonicalize to resolve symlinks and relative components (blocking I/O).
    let canonical_path = match tokio::task::spawn_blocking(move || {
        std::fs::canonicalize(&requested_path)
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, path = %manifest_path_str, "Cannot resolve tool manifest path");
            return (StatusCode::BAD_REQUEST, "Cannot resolve manifest path").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "Canonicalize task panicked");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    // Step 2: Check that canonicalized path starts with an allowed directory.
    // allowed_tool_dirs are pre-canonicalized at startup — pure in-memory comparison, no I/O.
    let allowed = state
        .allowed_tool_dirs
        .iter()
        .any(|d| canonical_path.starts_with(d));

    if !allowed {
        tracing::warn!(
            path = %canonical_path.display(),
            "Tool install blocked: path not in allowed directories"
        );
        let audit = state.kernel.audit.clone();
        let path_str = canonical_path.display().to_string();
        match tokio::task::spawn_blocking(move || {
            audit.append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: agentos_types::TraceID::new(),
                event_type: agentos_audit::AuditEventType::PermissionDenied,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "action": "tool_install_blocked",
                    "canonical_path": path_str,
                    "reason": "path_not_in_allowed_dirs",
                }),
                severity: agentos_audit::AuditSeverity::Security,
                reversible: false,
                rollback_ref: None,
            })
        })
        .await
        {
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "Failed to write audit entry for blocked tool install")
            }
            Err(e) => tracing::error!(error = %e, "Audit spawn_blocking panicked"),
            _ => {}
        }
        return (
            StatusCode::FORBIDDEN,
            "Manifest path is not in an allowed tool directory",
        )
            .into_response();
    }

    // Step 3: Verify .toml extension (defense in depth).
    if canonical_path
        .extension()
        .map(|e| e != "toml")
        .unwrap_or(true)
    {
        tracing::warn!(
            path = %canonical_path.display(),
            "Tool install blocked: manifest does not have .toml extension"
        );
        return (
            StatusCode::BAD_REQUEST,
            "Manifest file must have a .toml extension",
        )
            .into_response();
    }

    // Step 4: Route through kernel command dispatch for trust tier validation and audit.
    match state
        .kernel
        .api_install_tool(canonical_path.to_string_lossy().to_string())
        .await
    {
        Ok(()) => axum::response::Redirect::to("/tools").into_response(),
        Err(msg) => {
            tracing::error!(path = %canonical_path.display(), error = %msg, "Failed to install tool");
            (
                StatusCode::BAD_REQUEST,
                format!("Failed to install tool: {}", msg),
            )
                .into_response()
        }
    }
}

pub async fn remove(State(state): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    match state.kernel.api_remove_tool(name.clone()).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(msg) if msg.to_lowercase().contains("not found") => StatusCode::NOT_FOUND,
        Err(msg) => {
            tracing::error!(tool = %name, error = %msg, "Failed to remove tool");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
