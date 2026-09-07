use crate::agent_inbox::AgentInbox;
use crate::agent_message_inbox::AgentMessageInbox;
use agentos_types::AgentID;
use std::sync::Arc;

/// Renders an O(1)-token inbox-awareness segment.
///
/// This is NOT appended to the system prompt any more. Unread counts are
/// dynamic, and the system prompt sits inside the Anthropic prompt-cache
/// prefix, so appending them busted the cached prefix whenever a notification
/// arrived. `render_line` is folded into the per-iteration `<turn_reminder>`
/// instead, which lives after every cache breakpoint.
///
/// Renders nothing (empty string) when both inboxes are empty — zero overhead
/// for idle agents.
pub struct InboxPromptRenderer {
    inbox: Arc<AgentInbox>,
    messages: Arc<AgentMessageInbox>,
}

impl InboxPromptRenderer {
    pub fn new(inbox: Arc<AgentInbox>, messages: Arc<AgentMessageInbox>) -> Self {
        Self { inbox, messages }
    }

    /// Returns the markdown segment for `agent_id`, or an empty string when
    /// both inboxes are idle. Called once per TASK (not per turn) from
    /// `context_injector::setup_task_context`.
    ///
    /// Never renders per-notification titles, subjects, or bodies — counts only.
    /// This keeps the prompt token cost O(1) in inbox depth and prevents
    /// sensitive bodies from leaking into the raw system prompt.
    /// One-line form for `<turn_reminder>`: counts only, no markdown heading
    /// and no imperative.
    ///
    /// The tool names live in the system prompt, so repeating "use
    /// `agent-inbox-list`" as the trailing message of every iteration is an
    /// active nudge to re-poll an inbox the agent may have already drained.
    /// The count is resolved once per task, so it is labelled as such rather
    /// than presented as live.
    pub async fn render_line(&self, agent_id: AgentID) -> String {
        let notif_count = self.inbox.unread_count(agent_id).await.unwrap_or(0);
        let msg_by_sender = self
            .messages
            .unread_by_sender(agent_id)
            .await
            .unwrap_or_default();

        if notif_count == 0 && msg_by_sender.is_empty() {
            return String::new();
        }

        let mut parts = Vec::new();
        if notif_count > 0 {
            parts.push(format!("{notif_count} unread notification(s)"));
        }
        if !msg_by_sender.is_empty() {
            let list = msg_by_sender
                .iter()
                .map(|(_, name, c)| format!("{name}({c})"))
                .collect::<Vec<_>>()
                .join(" ");
            parts.push(format!("msgs from {list}"));
        }
        format!("inbox (at task start): {}", parts.join(", "))
    }

    pub async fn render_segment(&self, agent_id: AgentID) -> String {
        let notif_count = self.inbox.unread_count(agent_id).await.unwrap_or(0);
        let msg_by_sender = self
            .messages
            .unread_by_sender(agent_id)
            .await
            .unwrap_or_default();

        if notif_count == 0 && msg_by_sender.is_empty() {
            return String::new();
        }

        let mut out = String::from("\n\n## Notifications\n");

        if notif_count > 0 {
            out.push_str(&format!("Unread notifications: {notif_count}\n"));
        }

        if !msg_by_sender.is_empty() {
            // Ordered by count DESC then name ASC (stable — important for prompt cache hits).
            let list = msg_by_sender
                .iter()
                .map(|(_, name, c)| format!("{name} ({c})"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("Unread messages from: {list}\n"));
        }

        out.push_str(
            "\nUse the `agent-inbox-list` tool to view notifications, \
             `agent-messages-list` to view messages.\n",
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_message_inbox::AgentMessageInbox;
    use agentos_types::{
        AgentInboxEntry, AgentInboxEntryID, AgentInboxKind, AgentMessageEntry, AgentMessageEntryID,
    };
    use chrono::Utc;
    use tempfile::tempdir;

    async fn make_renderer() -> (InboxPromptRenderer, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let inbox =
            Arc::new(AgentInbox::new(&dir.path().join("inbox.db"), 200).expect("AgentInbox::new"));
        let messages = Arc::new(
            AgentMessageInbox::new(&dir.path().join("messages.db"), 200)
                .expect("AgentMessageInbox::new"),
        );
        (InboxPromptRenderer::new(inbox, messages), dir)
    }

    fn notif_entry(agent_id: AgentID) -> AgentInboxEntry {
        AgentInboxEntry {
            id: AgentInboxEntryID::new(),
            agent_id,
            kind: AgentInboxKind::Scheduled,
            title: "scheduled task completed: echo".into(),
            body: serde_json::json!({}),
            ref_id: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
        }
    }

    fn msg_entry(from_id: AgentID, from_name: &str, to_id: AgentID) -> AgentMessageEntry {
        AgentMessageEntry {
            id: AgentMessageEntryID::new(),
            from_agent_id: from_id,
            from_agent_name: from_name.into(),
            to_agent_id: to_id,
            body: "hello".into(),
            reply_to: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
        }
    }

    #[tokio::test]
    async fn empty_inboxes_render_empty_string() {
        let (renderer, _dir) = make_renderer().await;
        let out = renderer.render_segment(AgentID::new()).await;
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn notifications_only_renders_count_line() {
        let (renderer, _dir) = make_renderer().await;
        let agent = AgentID::new();
        renderer.inbox.write(&notif_entry(agent)).await.unwrap();
        renderer.inbox.write(&notif_entry(agent)).await.unwrap();
        renderer.inbox.write(&notif_entry(agent)).await.unwrap();
        let out = renderer.render_segment(agent).await;
        assert!(out.contains("Unread notifications: 3"), "got: {out}");
        assert!(!out.contains("Unread messages from"), "got: {out}");
        assert!(out.contains("agent-inbox-list"), "got: {out}");
    }

    #[tokio::test]
    async fn messages_only_groups_by_sender_ordered_count_desc() {
        let (renderer, _dir) = make_renderer().await;
        let to_agent = AgentID::new();

        // triager: 3 messages, researcher: 2, planner: 1
        let triager_id = AgentID::new();
        let researcher_id = AgentID::new();
        let planner_id = AgentID::new();

        for _ in 0..3 {
            renderer
                .messages
                .write(&msg_entry(triager_id, "triager", to_agent))
                .await
                .unwrap();
        }
        for _ in 0..2 {
            renderer
                .messages
                .write(&msg_entry(researcher_id, "researcher", to_agent))
                .await
                .unwrap();
        }
        renderer
            .messages
            .write(&msg_entry(planner_id, "planner", to_agent))
            .await
            .unwrap();

        let out = renderer.render_segment(to_agent).await;
        assert!(
            out.contains("triager (3), researcher (2), planner (1)"),
            "got: {out}"
        );
        assert!(!out.contains("Unread notifications"), "got: {out}");
    }

    #[tokio::test]
    async fn no_subjects_or_bodies_in_segment() {
        let (renderer, _dir) = make_renderer().await;
        let agent = AgentID::new();
        let mut entry = notif_entry(agent);
        entry.title = "SENSITIVE_SUBJECT".into();
        entry.body = serde_json::json!({ "secret": "SENSITIVE_BODY" });
        renderer.inbox.write(&entry).await.unwrap();

        let out = renderer.render_segment(agent).await;
        assert!(!out.contains("SENSITIVE_SUBJECT"), "title leaked: {out}");
        assert!(!out.contains("SENSITIVE_BODY"), "body leaked: {out}");
    }

    #[tokio::test]
    async fn segment_stable_across_turns_when_counts_unchanged() {
        let (renderer, _dir) = make_renderer().await;
        let agent = AgentID::new();
        renderer.inbox.write(&notif_entry(agent)).await.unwrap();
        let out1 = renderer.render_segment(agent).await;
        let out2 = renderer.render_segment(agent).await;
        assert_eq!(out1, out2, "segment must be stable for prompt cache");
    }

    #[tokio::test]
    async fn combined_notifications_and_messages() {
        let (renderer, _dir) = make_renderer().await;
        let agent = AgentID::new();
        let sender = AgentID::new();

        renderer.inbox.write(&notif_entry(agent)).await.unwrap();
        renderer
            .messages
            .write(&msg_entry(sender, "alice", agent))
            .await
            .unwrap();
        renderer
            .messages
            .write(&msg_entry(sender, "alice", agent))
            .await
            .unwrap();

        let out = renderer.render_segment(agent).await;
        assert!(out.contains("Unread notifications: 1"), "got: {out}");
        assert!(out.contains("alice (2)"), "got: {out}");
        assert!(out.contains("agent-inbox-list"), "got: {out}");
        assert!(out.contains("agent-messages-list"), "got: {out}");
    }
}
