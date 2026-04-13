use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};

#[derive(Debug, Default, Deserialize)]
pub struct LogQuery {
    pub q: Option<String>,
    pub level: Option<String>,
    pub event_type: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

pub async fn page(
    State(state): State<AppState>,
    Query(query): Query<LogQuery>,
    jar: CookieJar,
) -> Response {
    let q = query.q.unwrap_or_default().to_lowercase();
    let level = query.level.unwrap_or_default().to_lowercase();
    let event_type = query.event_type.unwrap_or_default().to_lowercase();
    let from = query.from.unwrap_or_default();
    let to = query.to.unwrap_or_default();
    let from_dt = chrono::DateTime::parse_from_rfc3339(&from)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let to_dt = chrono::DateTime::parse_from_rfc3339(&to)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let mut results = Vec::new();
    if let Ok(file) = File::open(&state.kernel.config.audit.log_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };

            let sev = value
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase();
            let et = value
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase();
            let text = line.to_lowercase();
            let ts = value
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));

            if !q.is_empty() && !text.contains(&q) {
                continue;
            }
            if !level.is_empty() && !sev.contains(&level) {
                continue;
            }
            if !event_type.is_empty() && !et.contains(&event_type) {
                continue;
            }
            if let (Some(bound), Some(ts)) = (from_dt, ts) {
                if ts < bound {
                    continue;
                }
            }
            if let (Some(bound), Some(ts)) = (to_dt, ts) {
                if ts > bound {
                    continue;
                }
            }

            results.push(context! {
                timestamp => value.get("timestamp").and_then(|v| v.as_str()).unwrap_or_default(),
                severity => value.get("severity").and_then(|v| v.as_str()).unwrap_or_default(),
                event_type => value.get("event_type").and_then(|v| v.as_str()).unwrap_or_default(),
                line,
            });

            if results.len() >= 1000 {
                break;
            }
        }
    }

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Logs",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "Logs" },
        ],
        q,
        level,
        event_type,
        from,
        to,
        results,
        log_path => state.kernel.config.audit.log_path.clone(),
        csrf_token,
    };
    super::render(&state.templates, "logs.html", ctx)
}
