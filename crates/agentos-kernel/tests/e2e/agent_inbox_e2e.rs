//! Phase 5 integration tests for the agent inbox system.
//!
//! These tests exercise the full write → prompt → read/dismiss flow using real
//! SQLite stores, without requiring a full kernel boot.

use agentos_kernel::agent_inbox::AgentInbox;
use agentos_kernel::agent_inbox_prompt::InboxPromptRenderer;
use agentos_kernel::agent_inbox_writer::AgentInboxWriter;
use agentos_kernel::agent_message_inbox::AgentMessageInbox;
use agentos_types::{AgentID, TaskID};
use std::sync::Arc;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn setup(
    dir: &TempDir,
) -> (
    Arc<AgentInbox>,
    Arc<AgentMessageInbox>,
    AgentInboxWriter,
    InboxPromptRenderer,
) {
    let inbox =
        Arc::new(AgentInbox::new(&dir.path().join("inbox.db"), 200).expect("AgentInbox::new"));
    let messages = Arc::new(
        AgentMessageInbox::new(&dir.path().join("messages.db"), 200)
            .expect("AgentMessageInbox::new"),
    );
    let writer = AgentInboxWriter::new(inbox.clone(), messages.clone(), 7);
    let renderer = InboxPromptRenderer::new(inbox.clone(), messages.clone());
    (inbox, messages, writer, renderer)
}

// ─── writer → prompt pipeline ───────────────────────────────────────────────

#[tokio::test]
async fn scheduled_notification_appears_in_prompt() {
    let dir = TempDir::new().unwrap();
    let (_, _, writer, renderer) = setup(&dir);
    let agent = AgentID::new();
    let task = TaskID::new();

    writer
        .write_scheduled(
            agent,
            task,
            "summarize logs",
            true,
            serde_json::json!({"lines": 42}),
        )
        .await;

    let seg = renderer.render_segment(agent).await;
    assert!(seg.contains("Unread notifications: 1"), "segment: {seg}");
    assert!(seg.contains("agent-inbox-list"), "segment: {seg}");
    assert!(
        !seg.contains("summarize logs"),
        "title must not leak: {seg}"
    );
}

#[tokio::test]
async fn async_done_notification_appears_in_prompt() {
    let dir = TempDir::new().unwrap();
    let (_, _, writer, renderer) = setup(&dir);
    let parent = AgentID::new();
    let child_task = TaskID::new();

    writer
        .write_async_done(
            parent,
            child_task,
            "researcher",
            false,
            serde_json::json!({"error": "timeout"}),
        )
        .await;

    let seg = renderer.render_segment(parent).await;
    assert!(seg.contains("Unread notifications: 1"), "segment: {seg}");
}

#[tokio::test]
async fn message_appears_in_prompt_grouped_by_sender() {
    let dir = TempDir::new().unwrap();
    let (_, _, writer, renderer) = setup(&dir);
    let alice = AgentID::new();
    let bob = AgentID::new();

    for _ in 0..3 {
        writer
            .write_message(alice, "alice".into(), bob, "hello".into())
            .await;
    }

    let seg = renderer.render_segment(bob).await;
    assert!(seg.contains("alice (3)"), "segment: {seg}");
    assert!(seg.contains("agent-messages-list"), "segment: {seg}");
    assert!(!seg.contains("hello"), "body must not leak: {seg}");
}

#[tokio::test]
async fn idle_agent_gets_empty_segment() {
    let dir = TempDir::new().unwrap();
    let (_, _, _, renderer) = setup(&dir);
    let agent = AgentID::new();

    let seg = renderer.render_segment(agent).await;
    assert!(seg.is_empty(), "expected empty segment, got: {seg}");
}

// ─── idempotency (ref_id dedup) ──────────────────────────────────────────────

#[tokio::test]
async fn duplicate_scheduled_with_same_outcome_is_deduped() {
    let dir = TempDir::new().unwrap();
    let (inbox, _, writer, _) = setup(&dir);
    let agent = AgentID::new();
    let task = TaskID::new();

    writer
        .write_scheduled(agent, task, "job", true, serde_json::json!({}))
        .await;
    writer
        .write_scheduled(agent, task, "job", true, serde_json::json!({}))
        .await;

    // same task_id + same outcome → INSERT OR IGNORE, count stays 1
    assert_eq!(inbox.unread_count(agent).await.unwrap(), 1);
}

#[tokio::test]
async fn fail_then_success_produces_two_entries() {
    let dir = TempDir::new().unwrap();
    let (inbox, _, writer, _) = setup(&dir);
    let agent = AgentID::new();
    let task = TaskID::new();

    writer
        .write_scheduled(agent, task, "job", false, serde_json::json!({}))
        .await;
    writer
        .write_scheduled(agent, task, "job", true, serde_json::json!({}))
        .await;

    // Different outcome suffix in ref_id → two distinct rows
    assert_eq!(inbox.unread_count(agent).await.unwrap(), 2);
}

// ─── inbox store: list / get / mark_read / dismiss ──────────────────────────

#[tokio::test]
async fn inbox_list_returns_written_entries() {
    let dir = TempDir::new().unwrap();
    let (inbox, _, writer, _) = setup(&dir);
    let agent = AgentID::new();

    for i in 0..3u8 {
        writer
            .write_scheduled(
                agent,
                TaskID::new(),
                &format!("task-{i}"),
                true,
                serde_json::json!({}),
            )
            .await;
    }

    let list = inbox.list(agent, true, 10).await.unwrap();
    assert_eq!(list.len(), 3);
}

#[tokio::test]
async fn inbox_mark_read_removes_from_unread() {
    let dir = TempDir::new().unwrap();
    let (inbox, _, writer, renderer) = setup(&dir);
    let agent = AgentID::new();

    writer
        .write_scheduled(agent, TaskID::new(), "job", true, serde_json::json!({}))
        .await;

    let list = inbox.list(agent, true, 10).await.unwrap();
    assert_eq!(list.len(), 1);

    inbox.mark_read(list[0].id).await.unwrap();
    assert_eq!(inbox.unread_count(agent).await.unwrap(), 0);

    // Prompt segment must now be empty (no unread).
    assert!(renderer.render_segment(agent).await.is_empty());
}

#[tokio::test]
async fn inbox_dismiss_deletes_entry() {
    let dir = TempDir::new().unwrap();
    let (inbox, _, writer, _) = setup(&dir);
    let agent = AgentID::new();

    writer
        .write_scheduled(agent, TaskID::new(), "job", true, serde_json::json!({}))
        .await;

    let list = inbox.list(agent, false, 10).await.unwrap();
    let entry_id = list[0].id;

    inbox.dismiss(entry_id).await.unwrap();

    let list_after = inbox.list(agent, false, 10).await.unwrap();
    assert!(list_after.is_empty());
    assert!(inbox.get(entry_id).await.unwrap().is_none());
}

// ─── message inbox: list / get / mark_read / dismiss ─────────────────────────

#[tokio::test]
async fn message_inbox_list_read_dismiss_flow() {
    let dir = TempDir::new().unwrap();
    let (_, messages, writer, renderer) = setup(&dir);
    let alice = AgentID::new();
    let bob = AgentID::new();

    writer
        .write_message(alice, "alice".into(), bob, "hello bob".into())
        .await;

    // Before read: prompt shows alice (1)
    let seg = renderer.render_segment(bob).await;
    assert!(seg.contains("alice (1)"), "segment: {seg}");

    // List
    let list = messages.list(bob, true, 10).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].body, "hello bob");

    let msg_id = list[0].id;

    // Mark read
    messages.mark_read(msg_id).await.unwrap();
    assert!(renderer.render_segment(bob).await.is_empty());

    // Dismiss
    messages.dismiss(msg_id).await.unwrap();
    assert!(messages.list(bob, false, 10).await.unwrap().is_empty());
}

// ─── cross-agent isolation ───────────────────────────────────────────────────

#[tokio::test]
async fn notifications_are_scoped_to_owning_agent() {
    let dir = TempDir::new().unwrap();
    let (inbox, _, writer, _) = setup(&dir);
    let alice = AgentID::new();
    let bob = AgentID::new();

    writer
        .write_scheduled(
            alice,
            TaskID::new(),
            "alice-job",
            true,
            serde_json::json!({}),
        )
        .await;

    assert_eq!(inbox.unread_count(alice).await.unwrap(), 1);
    assert_eq!(inbox.unread_count(bob).await.unwrap(), 0);
}

#[tokio::test]
async fn messages_are_scoped_to_recipient() {
    let dir = TempDir::new().unwrap();
    let (_, messages, writer, _) = setup(&dir);
    let alice = AgentID::new();
    let bob = AgentID::new();
    let carol = AgentID::new();

    writer
        .write_message(alice, "alice".into(), bob, "hi bob".into())
        .await;

    assert_eq!(messages.unread_count(bob).await.unwrap(), 1);
    assert_eq!(messages.unread_count(carol).await.unwrap(), 0);
}

// ─── sweep / retention ───────────────────────────────────────────────────────

#[tokio::test]
async fn inbox_sweep_removes_expired_entries() {
    use chrono::{Duration, Utc};

    let dir = TempDir::new().unwrap();
    let inbox = Arc::new(AgentInbox::new(&dir.path().join("inbox.db"), 200).unwrap());
    let agent = AgentID::new();

    // Write an entry that expired yesterday.
    let mut entry = agentos_types::AgentInboxEntry {
        id: agentos_types::AgentInboxEntryID::new(),
        agent_id: agent,
        kind: agentos_types::AgentInboxKind::Scheduled,
        title: "old task".into(),
        body: serde_json::json!({}),
        ref_id: Some("old-ref".into()),
        created_at: Utc::now() - Duration::days(2),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        read: false,
    };
    inbox.write(&entry).await.unwrap();

    // Also write a fresh entry that must survive.
    entry.id = agentos_types::AgentInboxEntryID::new();
    entry.ref_id = Some("fresh-ref".into());
    entry.expires_at = Some(Utc::now() + Duration::days(7));
    inbox.write(&entry).await.unwrap();

    let pruned = inbox.sweep_expired().await.unwrap();
    assert!(pruned >= 1, "should have pruned at least 1 expired entry");
    assert_eq!(
        inbox.unread_count(agent).await.unwrap(),
        1,
        "fresh entry survives"
    );
}

#[tokio::test]
async fn message_inbox_sweep_removes_expired_messages() {
    use chrono::{Duration, Utc};

    let dir = TempDir::new().unwrap();
    let messages = Arc::new(AgentMessageInbox::new(&dir.path().join("messages.db"), 200).unwrap());
    let alice = AgentID::new();
    let bob = AgentID::new();

    let mut msg = agentos_types::AgentMessageEntry {
        id: agentos_types::AgentMessageEntryID::new(),
        from_agent_id: alice,
        from_agent_name: "alice".into(),
        to_agent_id: bob,
        body: "stale".into(),
        reply_to: None,
        created_at: Utc::now() - Duration::days(10),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        read: false,
    };
    messages.write(&msg).await.unwrap();

    // Write a fresh message that must survive.
    msg.id = agentos_types::AgentMessageEntryID::new();
    msg.expires_at = Some(Utc::now() + Duration::days(7));
    messages.write(&msg).await.unwrap();

    let pruned = messages.sweep_expired().await.unwrap();
    assert!(pruned >= 1, "should have pruned at least 1 expired message");
    assert_eq!(
        messages.unread_count(bob).await.unwrap(),
        1,
        "fresh message survives"
    );
}
