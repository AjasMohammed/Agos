use agentos_types::NotificationID;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NotificationFilter {
    pub unread_only: Option<bool>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NotificationResponseRequest {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NotificationSummary {
    #[schema(value_type = String)]
    pub id: NotificationID,
    pub subject: String,
    pub priority: String,
    pub read: bool,
    pub timestamp: String,
    /// Source label (e.g. "Kernel", agent name).
    #[serde(default)]
    pub from: String,
    /// Truncated body text for list views.
    #[serde(default)]
    pub body: String,
    /// Task that raised this message (e.g. the per-turn chat task for a
    /// blocking `ask-user`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// True for an unanswered `Question` — the agent is blocked on a reply.
    #[serde(default)]
    pub needs_response: bool,
    /// Numeric escalation id when this notification is an approval prompt
    /// raised by the escalation sink. Lets a client link the bell entry to the
    /// escalation queue entry it must act on — without it the row carries no
    /// machine-readable pointer back to the thing that is blocking the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_id: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blocking `ask-user` question is what lets a client render an inline
    /// answer box; both correlation fields must survive the wire.
    #[test]
    fn summary_round_trips_question_correlation_fields() {
        let json = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "subject": "Which file?",
            "priority": "normal",
            "read": false,
            "timestamp": "2026-08-28T00:00:00Z",
            "task_id": "abc",
            "needs_response": true,
        });
        let s: NotificationSummary = serde_json::from_value(json).expect("deserialize");
        assert_eq!(s.task_id.as_deref(), Some("abc"));
        assert!(s.needs_response);

        // Older payloads (plain notifications) stay decodable and default off.
        let plain = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000002",
            "subject": "Done",
            "priority": "normal",
            "read": true,
            "timestamp": "2026-08-28T00:00:00Z",
        });
        let s: NotificationSummary = serde_json::from_value(plain).expect("deserialize");
        assert_eq!(s.task_id, None);
        assert!(!s.needs_response);
        // `task_id: None` is skipped, not emitted as null.
        let out = serde_json::to_value(&s).expect("serialize");
        assert!(out.get("task_id").is_none());
    }
}

#[cfg(test)]
mod needs_response_tests {
    use chrono::{Duration, Utc};

    /// Mirrors the predicate in `kernel_impl::list_notifications`. An expired
    /// question was already auto-answered for the agent, so advertising it as
    /// answerable would have the user reply into a void.
    fn needs_response(
        is_question: bool,
        answered: bool,
        expires_at: Option<chrono::DateTime<Utc>>,
    ) -> bool {
        is_question && !answered && expires_at.is_none_or(|e| e > Utc::now())
    }

    #[test]
    fn only_live_unanswered_questions_need_a_response() {
        let future = Some(Utc::now() + Duration::minutes(5));
        let past = Some(Utc::now() - Duration::minutes(5));
        assert!(needs_response(true, false, future));
        assert!(
            needs_response(true, false, None),
            "no deadline = still open"
        );
        assert!(
            !needs_response(true, false, past),
            "timed out, auto-answered"
        );
        assert!(!needs_response(true, true, future), "already answered");
        assert!(!needs_response(false, false, future), "not a question");
    }
}
