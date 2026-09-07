use crate::state::AppState;
use axum::extract::{Form, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ReviewQuery {
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ProposalActionForm {
    pub proposal_id: String,
}

pub async fn list(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Query(query): axum::extract::Query<ReviewQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(100);
    let proposals = state
        .kernel
        .user_pref_proposal_store
        .list_pending(limit)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            context! {
                id => p.id,
                task_id => p.task_id.to_string(),
                agent_id => p.agent_id.to_string(),
                kind => format!("{:?}", p.kind),
                content => p.content,
                confidence => p.confidence,
                evidence => p.evidence,
                created_at => p.created_at.to_rfc3339(),
            }
        })
        .collect::<Vec<_>>();

    let stats = state.kernel.user_pref_proposal_store.stats().await.ok();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Preference Proposals",
        breadcrumbs => vec![
            context! { label => "System", href => "/management" },
            context! { label => "Preference Proposals" },
        ],
        proposals,
        stats,
        csrf_token,
    };
    super::render(&state.templates, "prefs.html", ctx)
}

pub async fn accept(
    State(state): State<AppState>,
    Form(form): Form<ProposalActionForm>,
) -> Response {
    let Some(p) = (match state
        .kernel
        .user_pref_proposal_store
        .get(&form.proposal_id)
        .await
    {
        Ok(v) => v,
        Err(_) => return Redirect::to("/prefs?error=load").into_response(),
    }) else {
        return Redirect::to("/prefs?error=missing").into_response();
    };

    if state
        .kernel
        .context_memory_store
        .write(
            &p.agent_id.to_string(),
            &format!("- {}", p.content),
            Some("user_pref_proposal_accept_web"),
        )
        .await
        .is_err()
    {
        return Redirect::to("/prefs?error=write").into_response();
    }

    let _ = state
        .kernel
        .user_pref_proposal_store
        .accept(&form.proposal_id)
        .await;
    Redirect::to("/prefs").into_response()
}

pub async fn reject(
    State(state): State<AppState>,
    Form(form): Form<ProposalActionForm>,
) -> Response {
    let _ = state
        .kernel
        .user_pref_proposal_store
        .reject(&form.proposal_id)
        .await;
    Redirect::to("/prefs").into_response()
}
