use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct ScratchpadQuery {
    pub agent: Option<String>,
    pub q: Option<String>,
    pub title: Option<String>,
}

async fn render_page(
    State(state): State<AppState>,
    query: ScratchpadQuery,
    jar: CookieJar,
) -> Response {
    let agents = {
        let reg = state.kernel.agent_registry.read().await;
        reg.list_all()
            .into_iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>()
    };

    let selected_agent = query
        .agent
        .clone()
        .or_else(|| agents.first().cloned())
        .unwrap_or_default();

    let pages = if selected_agent.is_empty() {
        Vec::new()
    } else {
        state
            .kernel
            .scratchpad_store
            .list_pages(&selected_agent)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                context! {
                    id => p.id,
                    title => p.title,
                    tags => p.tags.join(", "),
                    updated_at => p.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                }
            })
            .collect::<Vec<_>>()
    };

    let search_q = query.q.clone().unwrap_or_default();
    let results = if selected_agent.is_empty() || search_q.trim().is_empty() {
        Vec::new()
    } else {
        state
            .kernel
            .scratchpad_store
            .search(&selected_agent, &search_q, &[], 30)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                context! {
                    title => r.page.title,
                    rank => format!("{:.3}", r.rank),
                    snippet => r.snippet,
                }
            })
            .collect::<Vec<_>>()
    };

    let selected_title = query.title.clone().unwrap_or_default();
    let page_view = if selected_agent.is_empty() || selected_title.trim().is_empty() {
        None
    } else {
        match state
            .kernel
            .scratchpad_store
            .read_page(&selected_agent, &selected_title)
            .await
        {
            Ok(page) => {
                let backlinks = state
                    .kernel
                    .scratchpad_store
                    .get_backlinks(&selected_agent, &selected_title)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| {
                        context! {
                            title => p.title,
                            updated_at => p.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                        }
                    })
                    .collect::<Vec<_>>();
                Some(context! {
                    title => page.title,
                    content => page.content,
                    tags => page.tags.join(", "),
                    updated_at => page.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    backlinks,
                })
            }
            Err(_) => None,
        }
    };

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Scratchpad",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "Scratchpad" },
        ],
        agents,
        selected_agent,
        search_q,
        selected_title,
        pages,
        results,
        page_view,
        csrf_token,
    };
    super::render(&state.templates, "scratchpad.html", ctx)
}

pub async fn page(
    state: State<AppState>,
    Query(query): Query<ScratchpadQuery>,
    jar: CookieJar,
) -> Response {
    render_page(state, query, jar).await
}

pub async fn agent_page(
    state: State<AppState>,
    Path(name): Path<String>,
    Query(mut query): Query<ScratchpadQuery>,
    jar: CookieJar,
) -> Response {
    if query.agent.is_none() {
        query.agent = Some(name);
    }
    render_page(state, query, jar).await
}
