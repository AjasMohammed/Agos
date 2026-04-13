use crate::state::AppState;
use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct EscalationsQuery {
    pub include_resolved: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveEscalationForm {
    pub decision: String,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<EscalationsQuery>,
    jar: CookieJar,
) -> Response {
    let include_resolved = query.include_resolved.unwrap_or(false);
    let escalations = if include_resolved {
        state.kernel.escalation_manager.list_all().await
    } else {
        state.kernel.escalation_manager.list_pending().await
    }
    .into_iter()
    .map(|e| {
        context! {
            id => e.id,
            task_id => e.task_id.to_string(),
            agent_id => e.agent_id.to_string(),
            reason => format!("{:?}", e.reason),
            context_summary => e.context_summary,
            decision_point => e.decision_point,
            options => e.options,
            urgency => e.urgency,
            blocking => e.blocking,
            created_at => e.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            expires_at => e.expires_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            resolved => e.resolved,
            resolution => e.resolution.unwrap_or_default(),
            metadata => e.metadata,
        }
    })
    .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Escalations",
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Escalations" },
        ],
        escalations,
        include_resolved,
        csrf_token,
    };
    super::render(&state.templates, "escalations.html", ctx)
}

pub async fn resolve(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Form(form): Form<ResolveEscalationForm>,
) -> Response {
    let decision = form.decision.trim().to_string();
    if decision.is_empty() {
        return (StatusCode::BAD_REQUEST, "Decision is required").into_response();
    }

    let decision_lower = decision.to_ascii_lowercase();
    let approved = matches!(
        decision_lower.as_str(),
        "approve" | "approved" | "allow" | "allowed"
    );

    let Some((task_id, _agent_id, blocking)) = state
        .kernel
        .escalation_manager
        .resolve(id, decision.clone())
        .await
    else {
        return (
            StatusCode::NOT_FOUND,
            format!("Escalation {} not found or already resolved", id),
        )
            .into_response();
    };

    if blocking {
        if approved {
            if let Err(e) = state.kernel.scheduler.requeue(&task_id).await {
                tracing::warn!(task_id = %task_id, error = %e, "Failed to requeue task after escalation approval");
            }
        } else {
            let _ = state
                .kernel
                .scheduler
                .update_state_if_not_terminal(&task_id, agentos_types::TaskState::Failed)
                .await;
            state
                .kernel
                .background_pool
                .fail(
                    &task_id,
                    format!("Escalation {} denied with decision: {}", id, decision),
                )
                .await;
        }
    }

    Redirect::to("/escalations").into_response()
}
