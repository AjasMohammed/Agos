use crate::agent_inbox::AgentInbox;
use crate::agent_message_inbox::AgentMessageInbox;
use agentos_types::{
    AgentID, AgentInboxEntry, AgentInboxEntryID, AgentInboxKind, AgentMessageEntry,
    AgentMessageEntryID, TaskID,
};
use chrono::{Duration, Utc};
use std::sync::Arc;

pub struct AgentInboxWriter {
    inbox: Arc<AgentInbox>,
    message_inbox: Arc<AgentMessageInbox>,
    default_ttl_days: i64,
}

impl AgentInboxWriter {
    pub fn new(
        inbox: Arc<AgentInbox>,
        message_inbox: Arc<AgentMessageInbox>,
        default_ttl_days: i64,
    ) -> Self {
        Self {
            inbox,
            message_inbox,
            default_ttl_days,
        }
    }

    pub async fn write_scheduled(
        &self,
        agent_id: AgentID,
        task_id: TaskID,
        task_name: &str,
        success: bool,
        summary: serde_json::Value,
    ) {
        let title = format!(
            "scheduled task {}: {}",
            if success { "completed" } else { "failed" },
            task_name
        );
        // Include outcome in ref_id so a task that fails and then is retried to
        // success produces two distinct inbox entries rather than silently
        // deduplicating via INSERT OR IGNORE.
        let ref_id = format!("{}:{}", task_id, if success { "ok" } else { "fail" });
        self.write(
            agent_id,
            AgentInboxKind::Scheduled,
            title,
            summary,
            Some(ref_id),
        )
        .await;
    }

    pub async fn write_async_done(
        &self,
        parent_agent_id: AgentID,
        child_task_id: TaskID,
        child_agent_name: &str,
        success: bool,
        summary: serde_json::Value,
    ) {
        let title = format!(
            "sub-agent {}: {}",
            if success { "completed" } else { "failed" },
            child_agent_name
        );
        // Include outcome in ref_id so a child that fails and replays (via
        // checkpoint) as success is not silently deduped.
        let ref_id = format!("{}:{}", child_task_id, if success { "ok" } else { "fail" });
        self.write(
            parent_agent_id,
            AgentInboxKind::AsyncDone,
            title,
            summary,
            Some(ref_id),
        )
        .await;
    }

    // Wired in Phase 3 when event_dispatch.rs gains SubscriptionMode::Notify.
    #[allow(dead_code)]
    pub async fn write_event(
        &self,
        agent_id: AgentID,
        subscription_id: String,
        event_type: &str,
        event_payload: serde_json::Value,
    ) {
        let title = format!("event: {event_type}");
        self.write(
            agent_id,
            AgentInboxKind::Event,
            title,
            event_payload,
            Some(subscription_id),
        )
        .await;
    }

    // Wired in Phase 3 when timer_manager fires and the agent_inbox_writer is injected.
    #[allow(dead_code)]
    pub async fn write_timer(&self, agent_id: AgentID, timer_id: String, label: &str) {
        let title = format!("timer fired: {label}");
        self.write(
            agent_id,
            AgentInboxKind::Timer,
            title,
            serde_json::json!({ "timer_id": timer_id, "label": label }),
            Some(timer_id),
        )
        .await;
    }

    /// Persist an agent-to-agent message into the message inbox.
    /// Called by the kernel command handler before delivering via the bus.
    pub async fn write_message(
        &self,
        from_agent_id: AgentID,
        from_agent_name: String,
        to_agent_id: AgentID,
        body: String,
    ) {
        let now = Utc::now();
        let entry = AgentMessageEntry {
            id: AgentMessageEntryID::new(),
            from_agent_id,
            from_agent_name,
            to_agent_id,
            body,
            reply_to: None,
            created_at: now,
            expires_at: Some(now + Duration::days(self.default_ttl_days)),
            read: false,
        };
        if let Err(e) = self.message_inbox.write(&entry).await {
            tracing::warn!(
                error = %e,
                from = %from_agent_id,
                to = %to_agent_id,
                "AgentMessageInbox write failed"
            );
        }
    }

    async fn write(
        &self,
        agent_id: AgentID,
        kind: AgentInboxKind,
        title: String,
        body: serde_json::Value,
        ref_id: Option<String>,
    ) {
        let now = Utc::now();
        let entry = AgentInboxEntry {
            id: AgentInboxEntryID::new(),
            agent_id,
            kind,
            title: title.chars().take(120).collect(),
            body,
            ref_id,
            created_at: now,
            expires_at: Some(now + Duration::days(self.default_ttl_days)),
            read: false,
        };
        if let Err(e) = self.inbox.write(&entry).await {
            tracing::warn!(
                error = %e,
                %agent_id,
                "AgentInbox write failed"
            );
        }
    }
}
