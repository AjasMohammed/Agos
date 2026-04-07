use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use minijinja::context;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;

#[derive(Debug, Deserialize, Default)]
pub struct DagQuery {
    pub partial: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct DagNodeView {
    id: String,
    short_id: String,
    agent_id: String,
    agent_name: String,
    state: String,
    state_class: String,
    prompt_preview: String,
    created_at: String,
    spawn_depth: u8,
    child_count: usize,
    duration_label: String,
    parent_task_id: Option<String>,
    children: Vec<DagNodeView>,
}

#[derive(Debug, Clone)]
struct TaskViewRecord {
    id: agentos_types::TaskID,
    agent_id: agentos_types::AgentID,
    agent_name: String,
    state: agentos_types::TaskState,
    prompt_preview: String,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    spawn_depth: u8,
    parent_task_id: Option<agentos_types::TaskID>,
    finished_at: Option<DateTime<Utc>>,
}

pub async fn dag_view(State(state): State<AppState>, Query(query): Query<DagQuery>) -> Response {
    let nodes = match build_dag_nodes(&state).await {
        Ok(nodes) => nodes,
        Err(response) => return response,
    };

    if query.partial.unwrap_or(false) {
        let ctx = context! { nodes => nodes };
        return super::render(&state.templates, "partials/dag_tree.html", ctx);
    }

    let ctx = context! {
        page_title => "Task DAG",
        breadcrumbs => vec![
            context! { label => "Tasks", href => "/tasks" },
            context! { label => "Task DAG" },
        ],
        nodes => nodes,
    };
    super::render(&state.templates, "dag.html", ctx)
}

pub async fn dag_mermaid(State(state): State<AppState>) -> Response {
    let tasks = state.kernel.scheduler.list_agent_tasks().await;
    let child_map = state.kernel.scheduler.get_full_child_map().await;
    let task_map: HashMap<_, _> = tasks.iter().map(|task| (task.id, task)).collect();

    let mut mermaid = String::from("graph TD\n");
    for (parent_id, children) in child_map {
        for child_id in children {
            let parent_key = mermaid_key(parent_id);
            let child_key = mermaid_key(child_id);
            let parent_label = task_map
                .get(&parent_id)
                .map(|task| mermaid_label(&task.original_prompt, &task.state))
                .unwrap_or_else(|| "Unknown".to_string());
            let child_label = task_map
                .get(&child_id)
                .map(|task| mermaid_label(&task.original_prompt, &task.state))
                .unwrap_or_else(|| "Unknown".to_string());
            mermaid.push_str(&format!(
                "    {parent_key}[\"{parent_label}\"] --> {child_key}[\"{child_label}\"]\n"
            ));
        }
    }

    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        mermaid,
    )
        .into_response()
}

pub async fn task_event_stream(
    State(state): State<AppState>,
) -> Sse<
    axum::response::sse::KeepAliveStream<
        futures::stream::BoxStream<'static, Result<Event, Infallible>>,
    >,
> {
    let rx = state.task_event_tx.subscribe();
    let stream = stream::unfold(rx, move |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let event_name = match event.event_type {
                        crate::task_events::TaskEventType::StateChanged => "task-state-changed",
                        crate::task_events::TaskEventType::SubAgentSpawned => {
                            "task-sub-agent-spawned"
                        }
                        crate::task_events::TaskEventType::Completed => "task-completed",
                        crate::task_events::TaskEventType::Failed => "task-failed",
                        crate::task_events::TaskEventType::Cancelled => "task-cancelled",
                    };
                    let payload =
                        serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
                    return Some((Ok(Event::default().event(event_name).data(payload)), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Sse::new(stream.boxed()).keep_alive(KeepAlive::default())
}

async fn build_dag_nodes(state: &AppState) -> Result<Vec<DagNodeView>, Response> {
    let tasks = state.kernel.scheduler.list_agent_tasks().await;
    let child_map = state.kernel.scheduler.get_full_child_map().await;
    let records = build_task_records(state, tasks).await;
    let record_map: HashMap<_, _> = records
        .into_iter()
        .map(|record| (record.id, record))
        .collect();

    let root_ids = root_task_ids(&record_map);
    let nodes = root_ids
        .into_iter()
        .filter_map(|root_id| build_node(root_id, &record_map, &child_map, &mut HashSet::new()))
        .collect::<Vec<_>>();

    Ok(nodes)
}

async fn build_task_records(
    state: &AppState,
    tasks: Vec<agentos_types::AgentTask>,
) -> Vec<TaskViewRecord> {
    let mut agent_names = HashMap::new();
    {
        let registry = state.kernel.agent_registry.read().await;
        for task in &tasks {
            let agent_name = registry
                .get_by_id(&task.agent_id)
                .map(|agent| agent.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string());
            agent_names.insert(task.id, agent_name);
        }
    }

    let mut records = Vec::with_capacity(tasks.len());
    for task in tasks {
        let finished_at = state
            .kernel
            .scheduler
            .get_task_result(&task.id)
            .await
            .map(|result| result.completed_at);
        records.push(TaskViewRecord {
            id: task.id,
            agent_id: task.agent_id,
            agent_name: agent_names
                .remove(&task.id)
                .unwrap_or_else(|| task.agent_id.to_string()),
            state: task.state,
            prompt_preview: truncate_chars(&task.original_prompt, 120),
            created_at: task.created_at,
            started_at: task.started_at,
            spawn_depth: task.spawn_depth,
            parent_task_id: task.parent_task_id,
            finished_at,
        });
    }
    records
}

fn build_node(
    task_id: agentos_types::TaskID,
    record_map: &HashMap<agentos_types::TaskID, TaskViewRecord>,
    child_map: &HashMap<agentos_types::TaskID, Vec<agentos_types::TaskID>>,
    seen: &mut HashSet<agentos_types::TaskID>,
) -> Option<DagNodeView> {
    if !seen.insert(task_id) {
        return None;
    }

    let record = record_map.get(&task_id)?;
    let child_ids = child_map.get(&task_id).cloned().unwrap_or_default();
    let mut children = Vec::new();
    for child_id in child_ids.iter().copied() {
        if let Some(child) = build_node(child_id, record_map, child_map, seen) {
            children.push(child);
        }
    }

    Some(DagNodeView {
        id: record.id.to_string(),
        short_id: short_task_id(record.id),
        agent_id: record.agent_id.to_string(),
        agent_name: record.agent_name.clone(),
        state: format!("{:?}", record.state),
        state_class: task_state_badge_class(record.state).to_string(),
        prompt_preview: record.prompt_preview.clone(),
        created_at: record
            .created_at
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
        spawn_depth: record.spawn_depth,
        child_count: children.len(),
        duration_label: duration_label(record),
        parent_task_id: record.parent_task_id.map(|id| id.to_string()),
        children,
    })
}

fn root_task_ids(
    record_map: &HashMap<agentos_types::TaskID, TaskViewRecord>,
) -> Vec<agentos_types::TaskID> {
    let mut root_ids = record_map
        .values()
        .filter(|record| {
            record
                .parent_task_id
                .map(|parent_id| !record_map.contains_key(&parent_id))
                .unwrap_or(true)
        })
        .map(|record| record.id)
        .collect::<Vec<_>>();
    root_ids.sort_by_key(|id| {
        record_map
            .get(id)
            .map(|record| record.created_at)
            .unwrap_or_else(Utc::now)
    });
    root_ids
}

fn duration_label(record: &TaskViewRecord) -> String {
    let start = record.started_at.unwrap_or(record.created_at);
    let end = record.finished_at.unwrap_or_else(Utc::now);
    let elapsed = end.signed_duration_since(start);
    let seconds = elapsed.num_seconds().max(0);
    if record.state.is_terminal() {
        format!("{} elapsed", human_duration(seconds))
    } else {
        format!("{} live", human_duration(seconds))
    }
}

fn human_duration(seconds: i64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 86_400 {
        return format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60);
    }
    format!("{}d {}h", seconds / 86_400, (seconds % 86_400) / 3600)
}

pub fn task_state_badge_class(state: agentos_types::TaskState) -> &'static str {
    match state {
        agentos_types::TaskState::Queued => "badge-queued",
        agentos_types::TaskState::Running => "badge-running",
        agentos_types::TaskState::Waiting => "badge-waiting",
        agentos_types::TaskState::Suspended => "badge-suspended",
        agentos_types::TaskState::Complete => "badge-complete",
        agentos_types::TaskState::Failed => "badge-failed",
        agentos_types::TaskState::Cancelled => "badge-cancelled",
    }
}

fn mermaid_key(task_id: agentos_types::TaskID) -> String {
    format!("task_{}", task_id.to_string().replace('-', "_"))
}

fn mermaid_label(prompt: &str, state: &agentos_types::TaskState) -> String {
    let prompt = truncate_chars(prompt, 48).replace('"', "'");
    format!("{prompt} | {:?}", state)
}

fn short_task_id(task_id: agentos_types::TaskID) -> String {
    task_id.to_string().chars().take(8).collect()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    format!("{}...", value.chars().take(max_chars).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_state_badge_class() {
        assert_eq!(
            task_state_badge_class(agentos_types::TaskState::Queued),
            "badge-queued"
        );
        assert_eq!(
            task_state_badge_class(agentos_types::TaskState::Running),
            "badge-running"
        );
        assert_eq!(
            task_state_badge_class(agentos_types::TaskState::Complete),
            "badge-complete"
        );
        assert_eq!(
            task_state_badge_class(agentos_types::TaskState::Failed),
            "badge-failed"
        );
        assert_eq!(
            task_state_badge_class(agentos_types::TaskState::Cancelled),
            "badge-cancelled"
        );
    }

    #[test]
    fn test_mermaid_output_starts_with_graph() {
        let label = mermaid_label("parent task", &agentos_types::TaskState::Running);
        assert!(label.contains("Running"));
        assert!(mermaid_key(agentos_types::TaskID::new()).starts_with("task_"));
    }
}
