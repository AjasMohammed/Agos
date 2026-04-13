use crate::state::AppState;
use agentos_types::{PermissionOp, PermissionSet};
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CreateRoleForm {
    pub name: String,
    pub description: Option<String>,
    pub permissions: Option<String>,
}

fn parse_permissions_block(raw: &str) -> Result<PermissionSet, String> {
    let mut set = PermissionSet::new();

    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let (resource, flags) = match trimmed.rsplit_once(':') {
            Some((res, fl)) if !res.trim().is_empty() && !fl.trim().is_empty() => {
                (res.trim(), fl.trim().to_lowercase())
            }
            _ => {
                return Err(format!(
                    "Invalid permission at line {}. Expected resource:flags (e.g. fs:/tmp:rwx)",
                    idx + 1
                ))
            }
        };

        let mut read = false;
        let mut write = false;
        let mut execute = false;
        let mut query = false;
        let mut observe = false;

        for ch in flags.chars() {
            match ch {
                'r' => read = true,
                'w' => write = true,
                'x' => execute = true,
                'q' => query = true,
                'o' => observe = true,
                '-' => {}
                _ => {
                    return Err(format!(
                        "Invalid permission flag '{}' at line {}. Allowed: r w x q o",
                        ch,
                        idx + 1
                    ))
                }
            }
        }

        set.grant(resource.to_string(), read, write, execute, None);
        if query {
            set.grant_op(resource.to_string(), PermissionOp::Query, None);
        }
        if observe {
            set.grant_op(resource.to_string(), PermissionOp::Observe, None);
        }
    }

    Ok(set)
}

pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Response {
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
                permission_count => r.permissions.entries.len(),
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Roles",
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Roles" },
        ],
        roles,
        csrf_token,
    };
    super::render(&state.templates, "roles.html", ctx)
}

pub async fn detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
    jar: CookieJar,
) -> Response {
    let Some(role) = state.kernel.profile_manager.get(&name) else {
        return (StatusCode::NOT_FOUND, "Role not found").into_response();
    };

    let entries = role
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
                expires_at => p.expires_at.map(|v| v.format("%Y-%m-%d %H:%M:%S UTC").to_string()).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => format!("Role {}", role.name),
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Roles", href => "/roles" },
            context! { label => role.name.clone() },
        ],
        role => context! {
            name => role.name,
            description => role.description,
            created_at => role.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            deny_entries => role.permissions.deny_entries.clone(),
        },
        entries,
        csrf_token,
    };
    super::render(&state.templates, "role_detail.html", ctx)
}

pub async fn create(State(state): State<AppState>, Form(form): Form<CreateRoleForm>) -> Response {
    let name = form.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Role name is required").into_response();
    }

    let description = form.description.as_deref().unwrap_or("").trim();
    let perms_raw = form.permissions.as_deref().unwrap_or("");
    let permission_set = match parse_permissions_block(perms_raw) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    match state
        .kernel
        .profile_manager
        .create(name, description, permission_set)
    {
        Ok(()) => Redirect::to("/roles").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to create role '{name}': {e}"),
        )
            .into_response(),
    }
}

pub async fn delete(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    match state.kernel.profile_manager.delete(&name) {
        Ok(()) => Redirect::to("/roles").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to delete role '{name}': {e}"),
        )
            .into_response(),
    }
}
