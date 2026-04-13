use crate::state::AppState;
use axum::extract::State;
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;
use std::collections::HashSet;

fn target_label(target: &agentos_types::MessageTarget) -> String {
    match target {
        agentos_types::MessageTarget::Direct(id) => format!("Direct:{}", id),
        agentos_types::MessageTarget::DirectByName(name) => format!("DirectByName:{}", name),
        agentos_types::MessageTarget::Group(id) => format!("Group:{}", id),
        agentos_types::MessageTarget::Broadcast => "Broadcast".to_string(),
    }
}

fn content_preview(content: &agentos_types::MessageContent) -> String {
    match content {
        agentos_types::MessageContent::Text(t) => t.chars().take(200).collect(),
        agentos_types::MessageContent::Structured(v) => serde_json::to_string(v)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
        agentos_types::MessageContent::TaskDelegation { prompt, .. } => {
            format!(
                "TaskDelegation: {}",
                prompt.chars().take(160).collect::<String>()
            )
        }
        agentos_types::MessageContent::TaskResult { task_id, .. } => {
            format!("TaskResult: {}", task_id)
        }
    }
}

pub async fn page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let online_agents = {
        let reg = state.kernel.agent_registry.read().await;
        reg.list_online()
            .into_iter()
            .map(|a| (a.id, a.name.clone()))
            .collect::<Vec<_>>()
    };

    let mut seen = HashSet::new();
    let mut rows: Vec<(chrono::DateTime<chrono::Utc>, minijinja::Value)> = Vec::new();

    for (agent_id, _name) in &online_agents {
        for msg in state.kernel.message_bus.get_history(agent_id, 100).await {
            let msg_id = msg.id.to_string();
            if !seen.insert(msg_id.clone()) {
                continue;
            }
            rows.push((
                msg.timestamp,
                context! {
                    id => msg_id,
                    from => msg.from.to_string(),
                    to => target_label(&msg.to),
                    timestamp => msg.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                    trace_id => msg.trace_id.to_string(),
                    preview => content_preview(&msg.content),
                },
            ));
        }
    }

    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let messages = rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "A2A",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "A2A" },
        ],
        messages,
        online_agents => online_agents.into_iter().map(|(id, name)| context!{ id => id.to_string(), name }).collect::<Vec<_>>(),
        csrf_token,
    };
    super::render(&state.templates, "a2a.html", ctx)
}
