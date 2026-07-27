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

/// A TaskLifecycle subscription must NOT fire for events about the
/// subscriber's own tasks (self-trigger = infinite loop), but must still
/// fire for other agents' task events.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn own_task_lifecycle_event_does_not_trigger_subscriber() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let watcher_id = common::register_mock_agent(&kernel, "watcher", vec![]).await;
    let worker_id = common::register_mock_agent(&kernel, "worker", vec![]).await;

    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "watcher".to_string(),
            event_filter: "category:TaskLifecycle".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    // Emit the watcher's OWN TaskFailed first, then the worker's. Events are
    // processed in order, so once the worker-triggered task appears the
    // dispatcher has provably drained past the watcher event — no sleep-and-
    // hope. The watcher must then have no reaction task at all.
    kernel
        .emit_event(
            EventType::TaskFailed,
            EventSource::TaskScheduler,
            EventSeverity::Warning,
            serde_json::json!({
                "task_id": agentos_types::TaskID::new().to_string(),
                "agent_id": watcher_id.to_string(),
                "reason": "llm_error",
                "error": "boom",
            }),
            0,
        )
        .await;
    kernel
        .emit_event(
            EventType::TaskFailed,
            EventSource::TaskScheduler,
            EventSeverity::Warning,
            serde_json::json!({
                "task_id": agentos_types::TaskID::new().to_string(),
                "agent_id": worker_id.to_string(),
                "reason": "llm_error",
                "error": "boom",
            }),
            0,
        )
        .await;

    let mut worker_triggered = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        if kernel
            .scheduler
            .list_tasks()
            .await
            .iter()
            .any(|s| s.agent_id == watcher_id)
        {
            worker_triggered = true;
            break;
        }
    }
    assert!(
        worker_triggered,
        "another agent's TaskFailed must still trigger the watcher's subscription"
    );
    // The watcher's reaction task must be about the WORKER's failure only; its
    // own TaskFailed (processed earlier) must have spawned nothing.
    let watcher_tasks: Vec<_> = kernel
        .scheduler
        .list_tasks()
        .await
        .into_iter()
        .filter(|s| s.agent_id == watcher_id)
        .collect();
    assert_eq!(
        watcher_tasks.len(),
        1,
        "watcher must get exactly one reaction task (worker's failure), not one for its own"
    );
    let task = kernel
        .scheduler
        .get_task(&watcher_tasks[0].id)
        .await
        .expect("triggered task must exist");
    assert!(
        task.original_prompt.contains(&worker_id.to_string()),
        "reaction task must be about the worker's failure; prompt: {}",
        task.original_prompt
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Pins the regression this file exists for: lifecycle events emitted by an
/// event-triggered task must carry `trigger_depth + 1`, not a hardcoded 0.
///
/// Chain: DiskSpaceLow(depth 0) → worker reaction task (stores depth 0) →
/// mock LLM completes it → TaskCompleted must be emitted at depth 1 →
/// watcher's TaskLifecycle subscription fires → watcher's reaction task
/// stores `trigger_source.chain_depth == 1`, which is observable via the
/// scheduler. If any emit site regresses to 0, this asserts 0 != 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn triggered_task_lifecycle_events_carry_incremented_depth() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let worker_id =
        common::register_mock_agent(&kernel, "depth-worker", vec!["disk handled".to_string()])
            .await;
    let watcher_id = common::register_mock_agent(&kernel, "depth-watcher", vec![]).await;

    for (agent, filter) in [
        ("depth-worker", "category:SystemHealth"),
        ("depth-watcher", "category:TaskLifecycle"),
    ] {
        let resp = client
            .send_command(KernelCommand::EventSubscribe {
                agent_name: agent.to_string(),
                event_filter: filter.to_string(),
                payload_filter: None,
                throttle: None,
                priority: None,
            })
            .await
            .expect("send EventSubscribe");
        assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));
    }

    kernel
        .emit_event(
            EventType::DiskSpaceLow,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({ "mounts": [] }),
            0,
        )
        .await;

    // Wait for the watcher's reaction task — it only exists once the worker's
    // triggered task ran to completion and its TaskCompleted was dispatched.
    let mut watcher_task: Option<agentos_types::AgentTask> = None;
    for _ in 0..150 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let summaries = kernel.scheduler.list_tasks().await;
        for s in summaries.iter().filter(|s| s.agent_id == watcher_id) {
            if let Some(t) = kernel.scheduler.get_task(&s.id).await {
                if t.trigger_source.is_some() {
                    watcher_task = Some(t);
                    break;
                }
            }
        }
        if watcher_task.is_some() {
            break;
        }
    }
    let watcher_task =
        watcher_task.expect("worker task completion did not trigger the watcher within 15s");
    let ts = watcher_task.trigger_source.as_ref().unwrap();
    assert_eq!(
        ts.chain_depth, 1,
        "the worker's lifecycle event must carry depth 1 (trigger 0 + 1); \
         a 0 here means an emit site regressed to hardcoded depth (event: {:?})",
        ts.event_type
    );
    // Sanity: it fired for the worker's activity, not something else.
    assert!(
        watcher_task
            .original_prompt
            .contains(&worker_id.to_string()),
        "watcher reaction must be about the worker's task"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Events past `max_chain_depth` are dropped by the dispatcher — the loop
/// guard that terminates trigger→task→event→trigger cascades.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn event_past_max_chain_depth_is_dropped() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let _sub_id = common::register_mock_agent(&kernel, "chained", vec![]).await;
    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "chained".to_string(),
            event_filter: "category:SystemHealth".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    let tasks_before = kernel.scheduler.list_tasks().await.len();

    // Boundary: depth == max is still delivered (guard is strictly `>`).
    kernel
        .emit_event(
            EventType::DiskSpaceLow,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({ "mounts": [] }),
            kernel.event_bus.max_chain_depth(),
        )
        .await;
    let mut delivered = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        if kernel.scheduler.list_tasks().await.len() > tasks_before {
            delivered = true;
            break;
        }
    }
    assert!(
        delivered,
        "event at exactly max_chain_depth must be delivered"
    );
    let tasks_at_boundary = kernel.scheduler.list_tasks().await.len();

    // One past the boundary: dropped. The boundary event above already proved
    // the dispatcher is draining, so a short wait here is meaningful.
    kernel
        .emit_event(
            EventType::DiskSpaceLow,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({ "mounts": [] }),
            kernel.event_bus.max_chain_depth() + 1,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        kernel.scheduler.list_tasks().await.len(),
        tasks_at_boundary,
        "events beyond max_chain_depth must be dropped, not delivered"
    );

    kernel.shutdown();
    handle.await.unwrap();
}
