use crate::state::AppState;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct EventsQuery {
    pub q: Option<String>,
    pub event_type: Option<String>,
    pub agent_id: Option<String>,
    pub limit: Option<u32>,
}

pub async fn page(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
    jar: CookieJar,
) -> Response {
    // Build a quick (id → name) map so subscription rows can show the
    // human-readable agent name instead of a raw UUID.
    let agent_name_by_id: std::collections::HashMap<String, String> = {
        let reg = state.kernel.agent_registry.read().await;
        reg.list_all()
            .into_iter()
            .map(|a| (a.id.to_string(), a.name.clone()))
            .collect()
    };

    let subscriptions = state
        .kernel
        .event_bus
        .list_subscriptions()
        .await
        .into_iter()
        .map(|s| {
            let agent_id_str = s.agent_id.to_string();
            let agent_name = agent_name_by_id
                .get(&agent_id_str)
                .cloned()
                .unwrap_or_else(|| "(unknown)".to_string());
            context! {
                id => s.id.to_string(),
                agent_id => agent_id_str,
                agent_name => agent_name,
                event_type_filter => format!("{:?}", s.event_type_filter),
                filter => s.filter.unwrap_or_default(),
                enabled => s.enabled,
                created_at => s.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                throttle_policy => format!("{:?}", s.throttle),
                priority => format!("{:?}", s.priority),
            }
        })
        .collect::<Vec<_>>();

    let q = query.q.unwrap_or_default().to_lowercase();
    let event_type_filter = query.event_type.unwrap_or_default().to_lowercase();
    let agent_id_filter = query.agent_id.unwrap_or_default().to_lowercase();
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let fetch_limit = limit.saturating_mul(5).min(5000);

    let mut event_rows = {
        let audit = state.kernel.audit.clone();
        tokio::task::spawn_blocking(move || audit.query_recent(fetch_limit).unwrap_or_default())
            .await
            .unwrap_or_default()
    }
    .into_iter()
    .filter(|entry| entry.event_type == agentos_audit::AuditEventType::EventEmitted)
    .filter(|entry| {
        if !q.is_empty() {
            let details = entry.details.to_string().to_lowercase();
            if !details.contains(&q) {
                return false;
            }
        }
        true
    })
    .filter(|entry| {
        if event_type_filter.is_empty() {
            true
        } else {
            entry
                .details
                .get("event_type")
                .and_then(|v| v.as_str())
                .map(|v| v.to_lowercase().contains(&event_type_filter))
                .unwrap_or(false)
        }
    })
    .filter(|entry| {
        if agent_id_filter.is_empty() {
            true
        } else {
            entry
                .agent_id
                .map(|id| id.to_string().to_lowercase().contains(&agent_id_filter))
                .unwrap_or(false)
        }
    })
    .map(|entry| {
        context! {
            timestamp => entry.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            trace_id => entry.trace_id.to_string(),
            event_type => entry.details.get("event_type").and_then(|v| v.as_str()).unwrap_or_default(),
            event_source => entry.details.get("source").and_then(|v| v.as_str()).unwrap_or_default(),
            severity => format!("{:?}", entry.severity),
            agent_id => entry.agent_id.map(|id| id.to_string()).unwrap_or_default(),
            task_id => entry.task_id.map(|id| id.to_string()).unwrap_or_default(),
            details => entry.details,
        }
    })
    .collect::<Vec<_>>();
    if event_rows.len() > limit as usize {
        event_rows.truncate(limit as usize);
    }

    let categories = agentos_kernel::event_permissions::ALL_EVENT_CATEGORIES
        .iter()
        .map(|cat| {
            context! {
                name => format!("{:?}", cat),
                permission => agentos_kernel::event_permissions::permission_for_category(*cat),
            }
        })
        .collect::<Vec<_>>();

    // Per-agent subscribable-category report — drives the "Subscribe Agent"
    // form so operators can see which categories the agent already has
    // `events.<cat>:observe` for. Operators can still subscribe an agent to a
    // category they lack observe on (operator override), but the UI shows the
    // current state so they can grant the role first if they prefer.
    let agents = {
        let reg = state.kernel.agent_registry.read().await;
        reg.list_all()
            .into_iter()
            .map(|agent| {
                let perms = reg.compute_effective_permissions(&agent.id);
                let report = agentos_kernel::event_permissions::subscribable_categories(&perms);
                let allowed_n = report.iter().filter(|(_, _, ok)| *ok).count();
                let cat_status = report
                    .into_iter()
                    .map(|(cat, resource, allowed)| {
                        context! {
                            name => format!("{:?}", cat),
                            permission => resource,
                            allowed => allowed,
                        }
                    })
                    .collect::<Vec<_>>();
                context! {
                    id => agent.id.to_string(),
                    name => agent.name.clone(),
                    roles => agent.roles.clone(),
                    status => format!("{:?}", agent.status),
                    category_status => cat_status,
                    allowed_category_count => allowed_n,
                }
            })
            .collect::<Vec<_>>()
    };

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Events",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "Events" },
        ],
        subscriptions,
        categories,
        agents,
        event_rows,
        q,
        event_type_filter,
        agent_id_filter,
        limit,
        csrf_token,
    };
    super::render(&state.templates, "events_log.html", ctx)
}

// ─────────────────────────────────────────────────────────────────────────────
// POST handlers — operator-side subscription management and test-event emit.
// All routes mounted under the CSRF-protected web layer.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateSubscriptionForm {
    pub agent_name: String,
    pub event_filter: String,
    #[serde(default)]
    pub payload_filter: Option<String>,
    #[serde(default)]
    pub throttle: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
}

pub async fn create_subscription(
    State(state): State<AppState>,
    Form(form): Form<CreateSubscriptionForm>,
) -> Response {
    let agent_name = form.agent_name.trim();
    let event_filter = form.event_filter.trim();
    if agent_name.is_empty() || event_filter.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "agent_name and event_filter required",
        )
            .into_response();
    }
    let normalize = |o: Option<String>| -> Option<String> {
        o.map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "none")
    };
    match state
        .kernel
        .cmd_event_subscribe(
            agent_name.to_string(),
            event_filter.to_string(),
            normalize(form.payload_filter),
            normalize(form.throttle),
            normalize(form.priority),
        )
        .await
    {
        agentos_bus::message::KernelResponse::EventSubscriptionId(_) => {
            Redirect::to("/events#subscriptions").into_response()
        }
        agentos_bus::message::KernelResponse::Error { message } => {
            (StatusCode::BAD_REQUEST, message).into_response()
        }
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unexpected response: {other:?}"),
        )
            .into_response(),
    }
}

pub async fn delete_subscription(
    State(state): State<AppState>,
    Path(subscription_id): Path<String>,
) -> Response {
    match state
        .kernel
        .cmd_event_unsubscribe(subscription_id.clone())
        .await
    {
        agentos_bus::message::KernelResponse::Success { .. } => {
            Redirect::to("/events#subscriptions").into_response()
        }
        agentos_bus::message::KernelResponse::Error { message } => {
            (StatusCode::BAD_REQUEST, message).into_response()
        }
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unexpected response: {other:?}"),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct EmitEventForm {
    pub event_type: String,
    #[serde(default)]
    pub severity: Option<String>,
    /// Optional JSON payload. Empty string ⇒ `{}`.
    #[serde(default)]
    pub payload: Option<String>,
}

/// Operator-initiated event emission — useful for testing subscriptions
/// without waiting for a real system condition (e.g. forcing a fake
/// `DiskSpaceLow` to verify a sysops agent reacts).
pub async fn emit_test_event(
    State(state): State<AppState>,
    Form(form): Form<EmitEventForm>,
) -> Response {
    let event_type = match agentos_kernel::event_bus::parse_event_type(form.event_type.trim()) {
        Some(et) => et,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "unknown event type '{}'. Use one from the catalog (e.g. DiskSpaceLow, CPUSpikeDetected).",
                    form.event_type
                ),
            )
                .into_response();
        }
    };
    let severity = match form.severity.as_deref().unwrap_or("warning") {
        "info" | "Info" => agentos_types::EventSeverity::Info,
        "warning" | "Warning" => agentos_types::EventSeverity::Warning,
        "critical" | "Critical" => agentos_types::EventSeverity::Critical,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                format!("unknown severity '{other}'. Use info/warning/critical."),
            )
                .into_response();
        }
    };
    let payload: serde_json::Value = match form.payload.as_deref() {
        None | Some("") => serde_json::json!({ "source": "web_ui_test_emit" }),
        Some(raw) => match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("payload must be JSON: {e}"),
                )
                    .into_response();
            }
        },
    };
    state
        .kernel
        .emit_event(
            event_type,
            agentos_types::EventSource::ExternalBridge,
            severity,
            payload,
            0,
        )
        .await;
    Redirect::to("/events#emit").into_response()
}
