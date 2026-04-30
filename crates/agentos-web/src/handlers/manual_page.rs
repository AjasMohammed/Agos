use crate::state::AppState;
use agentos_tools::agent_manual::AgentManualTool;
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
use axum::extract::{Query, State};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

const SECTIONS: &[(&str, &str)] = &[
    ("index", "Index"),
    ("tools", "Tools"),
    ("permissions", "Permissions"),
    ("memory", "Memory"),
    ("events", "Events"),
    ("commands", "Commands"),
    ("errors", "Errors"),
    ("feedback", "Feedback"),
    ("agents", "Agents"),
    ("tasks", "Tasks"),
    ("procedural", "Procedural"),
    ("escalation", "Escalation"),
    ("coordination", "Coordination"),
    ("scratchpad", "Scratchpad"),
    ("channels", "Channels"),
    ("mcp", "MCP"),
    ("hal", "HAL"),
    ("plugins", "Plugins"),
    ("skills", "Skills"),
    ("notifications", "Notifications"),
    ("containers", "Containers"),
    ("webhooks", "Webhooks"),
    ("capabilities", "Capabilities"),
];

fn section_nav() -> Vec<BTreeMap<&'static str, &'static str>> {
    SECTIONS
        .iter()
        .map(|(slug, label)| {
            let mut m = BTreeMap::new();
            m.insert("slug", *slug);
            m.insert("label", *label);
            m
        })
        .collect()
}

pub async fn page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Agent Manual",
        breadcrumbs => vec![
            context! { label => "Agent Manual" },
        ],
        sections => section_nav(),
        csrf_token,
    };
    super::render(&state.templates, "manual.html", ctx)
}

#[derive(Debug, Deserialize)]
pub struct ViewQuery {
    pub section: Option<String>,
    pub name: Option<String>,
    pub query: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub verbose: Option<bool>,
    pub raw: Option<bool>,
}

/// HTMX partial — runs the agent-manual tool with the requested section
/// and returns rendered HTML (visual or raw JSON).
pub async fn view(State(state): State<AppState>, Query(q): Query<ViewQuery>) -> Response {
    let section = q.section.unwrap_or_else(|| "index".to_string());

    let mut payload = serde_json::Map::new();
    payload.insert("section".into(), serde_json::Value::String(section.clone()));
    if let Some(name) = q.name.as_ref() {
        payload.insert("name".into(), serde_json::Value::String(name.clone()));
    }
    if let Some(query) = q.query.as_ref() {
        payload.insert("query".into(), serde_json::Value::String(query.clone()));
    }
    if let Some(page) = q.page {
        payload.insert("page".into(), serde_json::Value::from(page));
    }
    if let Some(page_size) = q.page_size {
        payload.insert("page_size".into(), serde_json::Value::from(page_size));
    }
    if let Some(category) = q.category.as_ref() {
        payload.insert(
            "category".into(),
            serde_json::Value::String(category.clone()),
        );
    }
    if let Some(tag) = q.tag.as_ref() {
        payload.insert("tag".into(), serde_json::Value::String(tag.clone()));
    }
    if let Some(verbose) = q.verbose {
        payload.insert("verbose".into(), serde_json::Value::Bool(verbose));
    }

    let manual = AgentManualTool::new_with_channels(
        state.kernel.tool_summaries.clone(),
        state.kernel.connected_channels_snapshot.clone(),
    );

    let ctx = ToolExecutionContext {
        data_dir: state.kernel.data_dir().to_path_buf(),
        task_id: TaskID::new(),
        agent_id: AgentID::new(),
        trace_id: TraceID::new(),
        permissions: PermissionSet::new(),
        vault: None,
        hal: None,
        file_lock_registry: None,
        agent_registry: None,
        task_registry: None,
        escalation_query: None,
        workspace_paths: vec![],
        capability_registry: None,
        capability_dispatcher: None,
        storage_zone_query: None,
        cancellation_token: CancellationToken::new(),
    };

    let result = manual
        .execute(serde_json::Value::Object(payload), ctx)
        .await;

    let raw = q.raw.unwrap_or(false);
    let template = if raw {
        "partials/manual_raw.html"
    } else {
        "partials/manual_section.html"
    };

    match result {
        Ok(value) => {
            let pretty = serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| "<serialization error>".into());
            let ctx = context! {
                section => section,
                name => q.name,
                query => q.query,
                data => value,
                pretty => pretty,
                raw => raw,
            };
            super::render(&state.templates, template, ctx)
        }
        Err(e) => {
            let ctx = context! {
                section => section,
                error => format!("{}", e),
            };
            super::render(&state.templates, "partials/manual_error.html", ctx)
        }
    }
}
