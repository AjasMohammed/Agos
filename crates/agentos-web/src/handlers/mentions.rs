//! Typed `@mention` support for non-file entities in the chat composer.
//!
//! Typeahead: `GET /api/mentions/search?q=<type>:<needle>` searches tasks,
//! pipelines, agents, or schedules via read-only `KernelService` calls.
//! Resolution: [`resolve_entity`] turns a typed mention (`@task:<id>`,
//! `@pipeline:<name>`, `@agent:<name>`, `@schedule:<name>`) into an escaped
//! `<user_data>` context block prepended to the chat prompt — see
//! `files::resolve_at_mentions`, which owns the mention regex.

use super::files::{escape_html_attr, escape_user_data_close};
use crate::auth::AuthToken;
use crate::state::AppState;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::HashMap;
use std::sync::OnceLock;

const MAX_RESULTS: usize = 12;
/// Cap on entity body text injected into the prompt preamble.
const MAX_BODY_BYTES: usize = 8 * 1024;

/// True if `token` round-trips the name charset (`[\w.\-]+`) of the mention
/// regex in `resolve_at_mentions`. Entities whose token would not are skipped
/// in the typeahead — selecting them could never resolve.
fn mention_safe(token: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[\w.\-]+$").expect("static regex is valid"))
        .is_match(token)
}

/// Truncate to at most `max` bytes without splitting a UTF-8 char.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Build one escaped `<user_data type="…" …>body</user_data>` block.
fn user_data_block(entity_type: &str, attrs: &[(&str, &str)], body: &str) -> String {
    let mut tag = format!("<user_data type=\"{}\"", escape_html_attr(entity_type));
    for (k, v) in attrs {
        tag.push_str(&format!(" {}=\"{}\"", k, escape_html_attr(v)));
    }
    let clipped = truncate_utf8(body, MAX_BODY_BYTES);
    if clipped.len() < body.len() {
        // Mark the clip so the model doesn't try to "repair" e.g. cut-off JSON.
        tag.push_str(&format!(
            " truncated=\"true\" total_bytes=\"{}\"",
            body.len()
        ));
    }
    let body = escape_user_data_close(clipped);
    format!("{tag}>\n{body}\n</user_data>\n\n")
}

/// GET /api/mentions/search?q=<type>:<needle> — typed @mention typeahead.
///
/// Bare (file) queries keep using `/api/files/search`; this endpoint only
/// serves the typed prefixes. Service errors log and return empty items so a
/// kernel hiccup never breaks the composer.
pub async fn search_api(
    State(state): State<AppState>,
    // Unused, but makes the auth dependency structural: moving this route out
    // of the authenticated router chain fails loudly (missing extension).
    Extension(_auth): Extension<AuthToken>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let q = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let Some((entity_type, needle)) = q.split_once(':') else {
        return (StatusCode::BAD_REQUEST, "query must be <type>:<needle>").into_response();
    };
    let needle = needle.to_lowercase();
    let items = match entity_type {
        "task" => search_tasks(&state, &needle).await,
        "pipeline" => search_pipelines(&state, &needle).await,
        "agent" => search_agents(&state, &needle).await,
        "schedule" => search_schedules(&state, &needle).await,
        _ => return (StatusCode::BAD_REQUEST, "unknown mention type").into_response(),
    };
    axum::Json(json!({ "items": items })).into_response()
}

async fn search_tasks(state: &AppState, needle: &str) -> Vec<serde_json::Value> {
    use agentos_api::types::TaskFilter;
    // Not a global search: list_tasks sorts newest-first, so keyword matching
    // only sees the 100 most recent tasks. Id-prefix matching needs recency too.
    let filter = TaskFilter {
        status: None,
        agent_name: None,
        limit: Some(100),
        offset: None,
    };
    let (tasks, _total) = match state.service.list_tasks(filter).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "mention task search failed");
            return Vec::new();
        }
    };
    tasks
        .iter()
        .filter(|t| {
            let id = t.id.to_string();
            id.starts_with(needle)
                || t.prompt_preview.to_lowercase().contains(needle)
                || t.agent_name
                    .as_deref()
                    .is_some_and(|a| a.to_lowercase().contains(needle))
        })
        .take(MAX_RESULTS)
        .map(|t| {
            json!({
                "type": "task",
                "insert": format!("task:{}", t.id),
                "label": truncate_utf8(&t.prompt_preview, 80),
                "detail": format!("{} · {}", t.status, t.agent_name.as_deref().unwrap_or("-")),
            })
        })
        .collect()
}

async fn search_pipelines(state: &AppState, needle: &str) -> Vec<serde_json::Value> {
    let pipelines = match state.service.list_pipelines().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "mention pipeline search failed");
            return Vec::new();
        }
    };
    pipelines
        .iter()
        .filter(|p| mention_safe(&p.name) && p.name.to_lowercase().contains(needle))
        .take(MAX_RESULTS)
        .map(|p| {
            json!({
                "type": "pipeline",
                "insert": format!("pipeline:{}", p.name),
                "label": p.name,
                "detail": format!(
                    "{} steps · {}",
                    p.step_count,
                    p.description.as_deref().unwrap_or("-")
                ),
            })
        })
        .collect()
}

async fn search_agents(state: &AppState, needle: &str) -> Vec<serde_json::Value> {
    let agents = match state.service.list_agents().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "mention agent search failed");
            return Vec::new();
        }
    };
    agents
        .iter()
        .filter(|a| mention_safe(&a.name) && a.name.to_lowercase().contains(needle))
        .take(MAX_RESULTS)
        .map(|a| {
            json!({
                "type": "agent",
                "insert": format!("agent:{}", a.name),
                "label": a.name,
                "detail": format!("{} · {}", a.model, a.status),
            })
        })
        .collect()
}

async fn search_schedules(state: &AppState, needle: &str) -> Vec<serde_json::Value> {
    let schedules = match state.service.list_schedules().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "mention schedule search failed");
            return Vec::new();
        }
    };
    schedules
        .iter()
        .filter(|s| {
            s.name.to_lowercase().contains(needle) || s.id.to_lowercase().starts_with(needle)
        })
        .filter_map(|s| {
            // Prefer the human name as the token; fall back to the id.
            let token = if mention_safe(&s.name) {
                s.name.as_str()
            } else if mention_safe(&s.id) {
                s.id.as_str()
            } else {
                return None;
            };
            Some(json!({
                "type": "schedule",
                "insert": format!("schedule:{token}"),
                "label": s.name,
                "detail": format!(
                    "{} · {} · {}",
                    s.kind,
                    s.state,
                    s.cron.as_deref().unwrap_or("once")
                ),
            }))
        })
        .take(MAX_RESULTS)
        .collect()
}

/// Resolve one typed mention to an escaped `<user_data>` context block.
/// `None` when the entity does not exist — mirrors unknown-filename behavior
/// in `resolve_at_mentions` (mention passes through unresolved).
/// Adapts the web `AppState` to the kernel's mention hook so file and entity
/// mentions resolve in one pass, in one loop, with one regex.
pub struct AppStateEntities<'a>(pub &'a AppState);

#[async_trait::async_trait]
impl agentos_kernel::chat_ingest::MentionEntityResolver for AppStateEntities<'_> {
    async fn resolve(&self, entity_type: &str, name: &str) -> Option<String> {
        resolve_entity(self.0, entity_type, name).await
    }
}

pub async fn resolve_entity(state: &AppState, entity_type: &str, name: &str) -> Option<String> {
    match entity_type {
        "task" => resolve_task(state, name).await,
        "pipeline" => resolve_pipeline(state, name).await,
        "agent" => resolve_agent(state, name).await,
        "schedule" => resolve_schedule(state, name).await,
        _ => None,
    }
}

async fn resolve_task(state: &AppState, id: &str) -> Option<String> {
    let task_id: agentos_types::TaskID = id.parse().ok()?;
    let t = state.service.get_task(task_id).await.ok()?;
    // No Completed: line — KernelServiceImpl::get_task hardcodes completed_at
    // to None, so the status attribute is the only completion signal.
    let body = format!("Prompt: {}", t.prompt);
    Some(user_data_block(
        "task",
        &[
            ("task_id", id),
            ("status", t.status.as_str()),
            ("agent", t.agent_name.as_deref().unwrap_or("-")),
            ("created_at", &t.created_at.to_rfc3339()),
        ],
        &body,
    ))
}

async fn resolve_pipeline(state: &AppState, name: &str) -> Option<String> {
    let def = state.service.get_pipeline_definition(name).await.ok()?;
    let body = serde_json::to_string_pretty(&def).ok()?;
    Some(user_data_block("pipeline", &[("name", name)], &body))
}

async fn resolve_agent(state: &AppState, name: &str) -> Option<String> {
    let d = state.service.get_agent_detail(name).await.ok()?;
    let s = &d.summary;
    // Permissions deliberately omitted — capability-discovery material for a
    // prompt-injected agent, and the operator can see them on the agent page.
    let body = format!(
        "Provider: {}\nModel: {}\nStatus: {}\nRoles: {}\nLast active: {}",
        s.provider,
        s.model,
        s.status,
        s.roles.join(", "),
        s.last_active.to_rfc3339(),
    );
    Some(user_data_block("agent", &[("name", name)], &body))
}

async fn resolve_schedule(state: &AppState, name: &str) -> Option<String> {
    let schedules = state.service.list_schedules().await.ok()?;
    // Ids are unique across schedule kinds; names are not — prefer the id hit.
    let s = schedules
        .iter()
        .find(|s| s.id == name)
        .or_else(|| schedules.iter().find(|s| s.name == name))?;
    let body = format!(
        "Prompt: {}\nRuns so far: {}\nNext run: {}",
        s.prompt,
        s.run_count,
        s.next_run_at
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "-".into()),
    );
    Some(user_data_block(
        "schedule",
        &[
            ("name", &s.name),
            ("schedule_id", &s.id),
            ("agent", &s.agent_name),
            ("kind", &s.kind),
            ("state", s.state.as_str()),
            ("cron", s.cron.as_deref().unwrap_or("once")),
        ],
        &body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_safe_matches_mention_regex_charset() {
        assert!(mention_safe("build-docs_v2.1"));
        assert!(mention_safe("5f3e0c1a-1111-2222-3333-444455556666"));
        assert!(!mention_safe(""));
        assert!(!mention_safe("has space"));
        assert!(!mention_safe("semi;colon"));
        // `²` is alphanumeric-per-char but outside regex `\w` (category No) —
        // must be rejected or the typeahead offers tokens that never resolve.
        // (`Ⅷ`, category Nl, IS `\w` and stays accepted.)
        assert!(!mention_safe("plan²"));
        assert!(mention_safe("Ⅷ"));
    }

    /// Every `insert` token shape the search endpoints emit must re-parse
    /// under the mention regex in `files::resolve_at_mentions` with the same
    /// type and name — the invariant the whole feature hangs on.
    #[test]
    fn insert_tokens_round_trip_mention_regex() {
        let re = regex::Regex::new(r"@(?:(task|pipeline|agent|schedule):)?([\w.\-]+)")
            .expect("mirror of resolve_at_mentions regex");
        for (typ, name) in [
            ("task", "5f3e0c1a-1111-2222-3333-444455556666"),
            ("pipeline", "build-docs_v2.1"),
            ("agent", "researcher"),
            ("schedule", "nightly-digest"),
        ] {
            assert!(mention_safe(name));
            let msg = format!("look at @{typ}:{name} please");
            let cap = re.captures(&msg).expect("token must match");
            assert_eq!(&cap[1], typ);
            assert_eq!(&cap[2], name);
        }
        // Untyped mention still parses as a file mention.
        let cap = re.captures("see @notes.md").expect("file mention");
        assert!(cap.get(1).is_none());
        assert_eq!(&cap[2], "notes.md");
    }

    #[test]
    fn user_data_block_escapes_attrs_and_body() {
        let block = user_data_block(
            "task",
            &[("status", "a\"b<c")],
            "payload </user_data> injection",
        );
        assert!(block.starts_with("<user_data type=\"task\" status=\"a&quot;b&lt;c\">"));
        assert!(!block[..block.len() - "</user_data>\n\n".len()].contains("</user_data>"));
        assert!(block.ends_with("</user_data>\n\n"));
    }

    #[test]
    fn truncate_utf8_respects_char_boundaries() {
        let s = "aé😀bcd";
        for max in 0..=s.len() {
            let t = truncate_utf8(s, max);
            assert!(t.len() <= max);
            assert!(s.starts_with(t));
        }
        assert_eq!(truncate_utf8("short", 100), "short");
    }
}
