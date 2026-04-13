use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use std::collections::HashMap;

pub async fn page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let all_tasks = state.kernel.scheduler.list_tasks().await;
    let mut child_count: HashMap<agentos_types::TaskID, usize> = HashMap::new();
    for t in &all_tasks {
        if let Some(parent_id) = t.parent_task_id {
            *child_count.entry(parent_id).or_insert(0) += 1;
        }
    }

    let team_tasks = state
        .kernel
        .scheduler
        .list_tasks()
        .await
        .into_iter()
        .filter(|t| t.is_team_coordinator)
        .map(|t| {
            context! {
                id => t.id.to_string(),
                state => format!("{:?}", t.state),
                agent_id => t.agent_id.to_string(),
                prompt_preview => t.prompt_preview,
                created_at => t.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                tool_calls => t.tool_calls,
                tokens_used => t.tokens_used,
                spawn_depth => t.spawn_depth,
                child_count => child_count.get(&t.id).copied().unwrap_or(0usize),
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Teams",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "Teams" },
        ],
        team_tasks,
        csrf_token,
    };
    super::render(&state.templates, "teams.html", ctx)
}

fn collect_children(
    parent_id: agentos_types::TaskID,
    depth: usize,
    by_parent: &HashMap<agentos_types::TaskID, Vec<agentos_types::TaskSummary>>,
    out: &mut Vec<(usize, agentos_types::TaskSummary)>,
) {
    let Some(children) = by_parent.get(&parent_id) else {
        return;
    };
    for child in children {
        out.push((depth, child.clone()));
        collect_children(child.id, depth + 1, by_parent, out);
    }
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Response {
    let task_id: agentos_types::TaskID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid team task ID").into_response(),
    };

    let all_tasks = state.kernel.scheduler.list_tasks().await;
    let mut by_id = HashMap::new();
    let mut by_parent: HashMap<agentos_types::TaskID, Vec<agentos_types::TaskSummary>> =
        HashMap::new();

    for task in all_tasks {
        if let Some(parent_id) = task.parent_task_id {
            by_parent.entry(parent_id).or_default().push(task.clone());
        }
        by_id.insert(task.id, task);
    }

    for children in by_parent.values_mut() {
        children.sort_by_key(|t| t.created_at);
    }

    let Some(root) = by_id.get(&task_id).cloned() else {
        return (StatusCode::NOT_FOUND, "Team coordinator task not found").into_response();
    };

    let mut hierarchy = Vec::new();
    collect_children(task_id, 1, &by_parent, &mut hierarchy);

    let rows = hierarchy
        .into_iter()
        .map(|(depth, t)| {
            context! {
                id => t.id.to_string(),
                state => format!("{:?}", t.state),
                agent_id => t.agent_id.to_string(),
                prompt_preview => t.prompt_preview,
                created_at => t.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                depth,
                spawn_depth => t.spawn_depth,
                tool_calls => t.tool_calls,
                tokens_used => t.tokens_used,
            }
        })
        .collect::<Vec<_>>();
    let descendant_count = rows.len();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => format!("Team {}", id),
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "Teams", href => "/teams" },
            context! { label => id.clone() },
        ],
        root => context! {
            id => root.id.to_string(),
            state => format!("{:?}", root.state),
            agent_id => root.agent_id.to_string(),
            prompt_preview => root.prompt_preview,
            created_at => root.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            spawn_depth => root.spawn_depth,
            tool_calls => root.tool_calls,
            tokens_used => root.tokens_used,
        },
        child_rows => rows,
        child_count => by_parent.get(&task_id).map(|v| v.len()).unwrap_or(0usize),
        descendant_count,
        csrf_token,
    };
    super::render(&state.templates, "teams_detail.html", ctx)
}
