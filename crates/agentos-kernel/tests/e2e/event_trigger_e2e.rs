//! End-to-end test: an emitted system-health event reaches a subscribed
//! agent as a triggered task and an inbox entry.
//!
//! This is the integration coverage that connects the three legs already
//! tested in isolation:
//!   1. `health_monitor::emit_event` (unit tests in `health_monitor.rs`)
//!   2. `EventBus::evaluate_subscriptions` (unit tests in `event_bus.rs`)
//!   3. `event_dispatch::process_event` → `create_triggered_task` (no prior unit test)
//!
//! We do not start the real `health_monitor` loop — that would require a HAL
//! threshold trip and a 30s wait. Instead we call `kernel.emit_event(...)`
//! directly with a `DiskSpaceLow` payload, which is exactly what the health
//! monitor itself does when the threshold is crossed.

use crate::common;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::{EventSeverity, EventSource, EventType};
use serial_test::serial;
use std::time::Duration;

/// Subscribe an agent to `SystemHealth`, emit a `DiskSpaceLow` event, and
/// confirm the dispatcher created a triggered task for the agent and wrote
/// a matching entry to the agent inbox.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn system_health_event_triggers_subscribed_agent() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    // Register a mock agent and look it up.
    let agent_id = common::register_mock_agent(&kernel, "sysops", vec![]).await;

    // Subscribe via the kernel command path so we exercise the same wiring
    // an operator would use from the CLI / web UI.
    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "sysops".to_string(),
            event_filter: "category:SystemHealth".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    let sub_id = match resp {
        KernelResponse::EventSubscriptionId(id) => id,
        other => panic!("Expected EventSubscriptionId, got: {other:?}"),
    };
    assert!(!sub_id.is_empty(), "subscription id must not be empty");

    // Baseline task count: triggered tasks should appear *after* emit.
    let tasks_before = kernel.scheduler.list_tasks().await.len();

    // Emit a DiskSpaceLow event — the same call shape `health_monitor` uses.
    kernel
        .emit_event(
            EventType::DiskSpaceLow,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({
                "mounts": [{
                    "mount_point": "/",
                    "disk_percent": 88.5,
                    "threshold": 85.0,
                }]
            }),
            0,
        )
        .await;

    // The dispatcher runs in a supervised task; give it a moment to consume.
    // Use TaskSummary.agent_id to narrow down, then fetch the full task to
    // confirm trigger_source. Loop with a short sleep until the dispatcher
    // has had time to enqueue.
    let mut triggered: Option<agentos_types::AgentTask> = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let summaries = kernel.scheduler.list_tasks().await;
        if summaries.len() > tasks_before {
            for s in summaries.iter().filter(|s| s.agent_id == agent_id) {
                if let Some(task) = kernel.scheduler.get_task(&s.id).await {
                    if task
                        .trigger_source
                        .as_ref()
                        .map(|ts| ts.event_type == EventType::DiskSpaceLow)
                        .unwrap_or(false)
                    {
                        triggered = Some(task);
                        break;
                    }
                }
            }
            if triggered.is_some() {
                break;
            }
        }
    }
    let task = triggered.expect("dispatcher did not create a triggered task within 2s");

    assert!(
        task.original_prompt.contains("DiskSpaceLow")
            || task.original_prompt.to_lowercase().contains("disk"),
        "trigger prompt should mention the event; got: {}",
        task.original_prompt
    );
    assert_eq!(task.spawn_depth, 0, "triggered tasks start at depth 0");

    // The agent inbox should have a corresponding event entry.
    let inbox_entries = kernel
        .agent_inbox
        .list(agent_id, false, 50)
        .await
        .expect("list inbox");
    assert!(
        inbox_entries
            .iter()
            .any(|e| e.title.contains("DiskSpaceLow")),
        "expected DiskSpaceLow inbox entry; got titles: {:?}",
        inbox_entries.iter().map(|e| &e.title).collect::<Vec<_>>()
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Emitting an event with no matching subscription does NOT create a task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unrelated_event_does_not_trigger_task() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let _agent_id = common::register_mock_agent(&kernel, "sysops2", vec![]).await;

    // Subscribe only to MemoryEvents.
    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "sysops2".to_string(),
            event_filter: "category:MemoryEvents".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    let tasks_before = kernel.scheduler.list_tasks().await.len();

    // Emit a SystemHealth event — should be dropped, not delivered.
    kernel
        .emit_event(
            EventType::CPUSpikeDetected,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({ "cpu_percent": 92.0, "threshold": 85.0 }),
            0,
        )
        .await;

    // Give the dispatcher 200ms; if no task appears, the filter held.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tasks_after = kernel.scheduler.list_tasks().await.len();
    assert_eq!(
        tasks_after, tasks_before,
        "no task should have been created for an unsubscribed category"
    );

    kernel.shutdown();
    handle.await.unwrap();
}
