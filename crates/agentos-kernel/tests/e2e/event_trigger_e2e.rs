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

/// An Offline agent (failed connect, auto-paused, or manually disconnected) has
/// no usable LLM adapter, so its subscriptions must not spawn triggered tasks —
/// the event is only recorded in its inbox.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn offline_agent_subscription_does_not_trigger_task() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let agent_id = common::register_mock_agent(&kernel, "deadagent", vec![]).await;

    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "deadagent".to_string(),
            event_filter: "category:SystemHealth".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    // Simulate a dead backend: the failure breaker / disconnect path sets this.
    kernel
        .agent_registry
        .write()
        .await
        .set_offline(&agent_id, true);

    let tasks_before = kernel.scheduler.list_tasks().await.len();

    kernel
        .emit_event(
            EventType::DiskSpaceLow,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({ "mounts": [] }),
            0,
        )
        .await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        kernel.scheduler.list_tasks().await.len(),
        tasks_before,
        "an Offline agent must not get a triggered task"
    );

    // ...but it still sees the event when it comes back.
    let inbox_entries = kernel
        .agent_inbox
        .list(agent_id, false, 50)
        .await
        .expect("list inbox");
    assert!(
        inbox_entries
            .iter()
            .any(|e| e.title.contains("DiskSpaceLow")),
        "offline agent should still have the event in its inbox; got: {:?}",
        inbox_entries.iter().map(|e| &e.title).collect::<Vec<_>>()
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// A direct message must trigger only its addressee. Every agent on a catch-all
/// role is seeded with an unfiltered `DirectMessageReceived` subscription, so
/// without recipient scoping one DM would spawn a full inference on every online
/// agent — and each "reply directly using agent-message" would fan out again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn direct_message_triggers_only_the_addressee() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let recipient_id = common::register_mock_agent(&kernel, "dm-recipient", vec![]).await;
    let bystander_id = common::register_mock_agent(&kernel, "dm-bystander", vec![]).await;
    let sender_id = common::register_mock_agent(&kernel, "dm-sender", vec![]).await;

    for name in ["dm-recipient", "dm-bystander", "dm-sender"] {
        let resp = client
            .send_command(KernelCommand::EventSubscribe {
                agent_name: name.to_string(),
                event_filter: "DirectMessageReceived".to_string(),
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
            EventType::DirectMessageReceived,
            EventSource::AgentMessageBus,
            EventSeverity::Info,
            serde_json::json!({
                "from_agent": sender_id.to_string(),
                "to_agent": recipient_id.to_string(),
                "message_id": agentos_types::MessageID::new().to_string(),
                "live_listener": false,
            }),
            0,
        )
        .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let summaries = kernel.scheduler.list_tasks().await;
    let count = |id| summaries.iter().filter(|s| s.agent_id == id).count();
    assert_eq!(count(recipient_id), 1, "addressee must be triggered once");
    assert_eq!(count(bystander_id), 0, "bystander must not be triggered");
    assert_eq!(count(sender_id), 0, "sender must not be triggered");

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

/// Minimal Ollama `/api/chat` stub: answers every request with the same
/// non-streamed assistant message. Enough for a connected agent's onboarding
/// task to get a real answer out of a real `OllamaCore`.
async fn spawn_ollama_stub() -> String {
    spawn_ollama_stub_with_delay(Duration::ZERO).await
}

/// As [`spawn_ollama_stub`], but waits `delay` before answering — a reaction
/// task whose backend takes that long is observably still in flight.
async fn spawn_ollama_stub_with_delay(delay: Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                // Read the whole request before replying — the onboarding prompt
                // plus tool manifests is far bigger than one read, and answering
                // early would reset the connection mid-upload.
                let mut buf = vec![0u8; 16 * 1024];
                let mut data: Vec<u8> = Vec::new();
                let (mut head, mut body_len) = (0usize, 0usize);
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    data.extend_from_slice(&buf[..n]);
                    if head == 0 {
                        if let Some(p) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                            head = p + 4;
                            let headers = String::from_utf8_lossy(&data[..p]).to_ascii_lowercase();
                            body_len = headers
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse().ok())
                                .unwrap_or(0);
                        }
                    }
                    if head > 0 && data.len() >= head + body_len {
                        break;
                    }
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let body = r#"{"model":"stub","message":{"role":"assistant","content":"Ready for tasks."},"done":true,"done_reason":"stop","total_duration":1,"prompt_eval_count":1,"eval_count":1}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// Peers subscribed to `AgentAdded` must only be triggered once the newly
/// connected agent's backend actually answers. Connecting an agent whose backend
/// is dead — the pre-flight health check skipped, exactly as `--no-health-check`
/// does — must never wake them; connecting one that answers must.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn agent_added_announced_only_after_backend_answers() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let watcher = common::register_mock_agent(&kernel, "watcher", vec![]).await;

    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "watcher".to_string(),
            event_filter: "AgentAdded".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(
        matches!(resp, KernelResponse::EventSubscriptionId(_)),
        "Expected EventSubscriptionId, got: {resp:?}"
    );

    let watcher_was_triggered = || async {
        for s in kernel.scheduler.list_tasks().await {
            if s.agent_id != watcher {
                continue;
            }
            if let Some(task) = kernel.scheduler.get_task(&s.id).await {
                if task
                    .trigger_source
                    .as_ref()
                    .is_some_and(|ts| ts.event_type == EventType::AgentAdded)
                {
                    return true;
                }
            }
        }
        false
    };

    // Port 1 is unreachable, so this agent's onboarding task never gets an answer.
    let resp = client
        .send_command(KernelCommand::ConnectAgent {
            name: "deadbeat".to_string(),
            provider: agentos_types::LLMProvider::Ollama,
            model: "llama3.2".to_string(),
            base_url: Some("http://127.0.0.1:1".to_string()),
            roles: vec![],
            test_mode: false,
            extra_permissions: vec![],
            root: false,
            skip_health_check: true,
        })
        .await
        .expect("send ConnectAgent");
    assert!(
        matches!(resp, KernelResponse::Success { .. }),
        "Expected Success on connect, got: {resp:?}"
    );

    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !watcher_was_triggered().await,
            "peer was triggered for an agent whose backend never answered"
        );
    }

    // Same path, but this backend replies — the peer must now be triggered.
    let stub = spawn_ollama_stub().await;
    let resp = client
        .send_command(KernelCommand::ConnectAgent {
            name: "livewire".to_string(),
            provider: agentos_types::LLMProvider::Ollama,
            model: "llama3.2".to_string(),
            base_url: Some(stub),
            roles: vec![],
            test_mode: false,
            extra_permissions: vec![],
            root: false,
            skip_health_check: true,
        })
        .await
        .expect("send ConnectAgent");
    assert!(
        matches!(resp, KernelResponse::Success { .. }),
        "Expected Success on connect, got: {resp:?}"
    );

    let mut triggered = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if watcher_was_triggered().await {
            triggered = true;
            break;
        }
    }
    assert!(
        triggered,
        "an agent whose backend answered should be announced to subscribed peers"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// A budget-exhausted agent must not be woken by SystemHealth events — its
/// own budget events re-matching its own subscription is a self-feeding
/// chain, and even host-caused events (DiskSpaceLow) can only produce a task
/// that suspends on its first inference. The event must still land in the
/// agent's inbox so it sees what it missed once the 24h period rolls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn budget_exhausted_agent_subscription_does_not_trigger_task() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let agent_id = common::register_mock_agent(&kernel, "brokeagent", vec![]).await;

    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "brokeagent".to_string(),
            event_filter: "category:SystemHealth".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    // Exhaust the agent's budget: tiny token cap, then record usage past it.
    kernel
        .cost_tracker
        .register_agent(
            agent_id,
            "brokeagent".to_string(),
            agentos_types::AgentBudget {
                max_tokens_per_day: 100,
                max_cost_usd_per_day: 0.0,
                max_tool_calls_per_day: 0,
                warn_at_pct: 80,
                pause_at_pct: 95,
                on_hard_limit: agentos_types::BudgetAction::Suspend,
                downgrade_model: None,
                allowed_models: Vec::new(),
                max_wall_time_seconds: 0,
            },
        )
        .await;
    kernel
        .cost_tracker
        .record_inference(
            &agent_id,
            &agentos_llm::TokenUsage {
                prompt_tokens: 150,
                completion_tokens: 50,
                total_tokens: 200,
            },
            "ollama",
            "mock-model",
        )
        .await;

    let tasks_before = kernel.scheduler.list_tasks().await.len();

    kernel
        .emit_event(
            EventType::DiskSpaceLow,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({ "mounts": [] }),
            0,
        )
        .await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        kernel.scheduler.list_tasks().await.len(),
        tasks_before,
        "a budget-exhausted agent must not get a triggered task"
    );

    let inbox_entries = kernel
        .agent_inbox
        .list(agent_id, false, 50)
        .await
        .expect("list inbox");
    assert!(
        inbox_entries
            .iter()
            .any(|e| e.title.contains("DiskSpaceLow")),
        "budget-paused agent should still have the event in its inbox; got: {:?}",
        inbox_entries.iter().map(|e| &e.title).collect::<Vec<_>>()
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Budget events never spawn a reaction task — not for the agent that tripped
/// them (the 2026-08-31 self-feeding chain), and not for any other subscriber
/// either. Budget is control-plane: only the operator can raise a cap or
/// resume an agent, so the operator is notified and every subscriber gets the
/// event in its inbox instead of a task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn budget_events_never_spawn_reaction_tasks() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let watcher_id = common::register_mock_agent(&kernel, "budgetwatcher", vec![]).await;
    let worker_id = common::register_mock_agent(&kernel, "budgetworker", vec![]).await;

    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "budgetwatcher".to_string(),
            event_filter: "category:SystemHealth".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    let tasks_before = kernel.scheduler.list_tasks().await.len();

    // The watcher's OWN budget event — self-exclusion must swallow it.
    kernel
        .emit_event(
            EventType::BudgetExhausted,
            EventSource::InferenceKernel,
            EventSeverity::Critical,
            serde_json::json!({
                "task_id": agentos_types::TaskID::new().to_string(),
                "agent_id": watcher_id.to_string(),
                "resource": "tokens",
                "action": "Suspend",
            }),
            0,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        kernel.scheduler.list_tasks().await.len(),
        tasks_before,
        "an agent must not be triggered by its own BudgetExhausted event"
    );

    // ANOTHER agent's budget event must not wake the watcher either — the
    // watcher cannot raise a cap or resume the worker; only the operator can.
    kernel
        .emit_event(
            EventType::BudgetExhausted,
            EventSource::InferenceKernel,
            EventSeverity::Critical,
            serde_json::json!({
                "task_id": agentos_types::TaskID::new().to_string(),
                "agent_id": worker_id.to_string(),
                "resource": "tokens",
                "action": "Suspend",
            }),
            0,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        kernel.scheduler.list_tasks().await.len(),
        tasks_before,
        "a budget event must not spawn a reaction task for any subscriber"
    );

    // Both budget events are still recorded in the watcher's inbox, so it can
    // see what happened on its next task without one being spawned for it.
    let inbox_entries = kernel
        .agent_inbox
        .list(watcher_id, false, 50)
        .await
        .expect("list inbox");
    assert_eq!(
        inbox_entries
            .iter()
            .filter(|e| e.title.contains("BudgetExhausted"))
            .count(),
        2,
        "both budget events belong in the subscriber's inbox; got: {:?}",
        inbox_entries.iter().map(|e| &e.title).collect::<Vec<_>>()
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// A subscription created without an explicit throttle must be stored with
/// the bounded default, not `ThrottlePolicy::None` — the fail-closed posture
/// added after the 2026-08-31 budget-loop incident.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unspecified_throttle_defaults_to_bounded_policy() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    common::register_mock_agent(&kernel, "throttledagent", vec![]).await;

    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "throttledagent".to_string(),
            event_filter: "category:SystemHealth".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    let resp = client
        .send_command(KernelCommand::EventListSubscriptions {
            agent_name: Some("throttledagent".to_string()),
        })
        .await
        .expect("send EventListSubscriptions");
    let subs = match resp {
        KernelResponse::EventSubscriptionList(subs) => subs,
        other => panic!("Expected EventSubscriptionList, got: {other:?}"),
    };
    assert_eq!(subs.len(), 1);
    let throttle = subs[0]["throttle"].as_str().unwrap_or_default().to_string();
    assert!(
        throttle.contains("MaxCountPerDuration"),
        "unspecified throttle must default to the bounded policy; got: {throttle}"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// `ProcessCrashed` carries `agent_id` = the OWNER of the dead process, not a
/// causer. It must still wake its subscriber — an agent supervising a spawned
/// daemon is exactly who needs the event. Regression guard for the first cut
/// of the budget-loop fix, which excluded all of `SystemHealth` by category
/// and silently broke process supervision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn own_process_crash_still_triggers_subscriber() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel().await;

    let agent_id = common::register_mock_agent(&kernel, "procwatcher", vec![]).await;

    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "procwatcher".to_string(),
            event_filter: "category:SystemHealth".to_string(),
            payload_filter: None,
            throttle: None,
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    let tasks_before = kernel.scheduler.list_tasks().await.len();

    // Same payload shape the process-crash callback emits: agent_id is the
    // process owner — the subscriber itself.
    kernel
        .emit_event(
            EventType::ProcessCrashed,
            EventSource::TaskScheduler,
            EventSeverity::Critical,
            serde_json::json!({
                "process_id": "proc-1",
                "agent_id": agent_id.to_string(),
                "task_id": agentos_types::TaskID::new().to_string(),
                "binary": "/usr/bin/daemon",
                "pid": 4242,
                "status": "Killed",
            }),
            0,
        )
        .await;

    let mut triggered = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        if kernel.scheduler.list_tasks().await.len() > tasks_before {
            triggered = true;
            break;
        }
    }
    assert!(
        triggered,
        "an agent must still be woken when its OWN managed process crashes"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// A burst of same-type events inside the batch window produces exactly ONE
/// reaction task carrying a digest of all of them, not one task per event.
/// This is the 2026-08-31 shape: an operator bulk-approved 30 devices and the
/// dispatcher spawned 60 tasks in two seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn burst_of_events_coalesces_into_one_reaction_task() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel_with(|cfg| {
        cfg.kernel.events.reaction_batch_window_secs = 1;
    })
    .await;

    let agent_id = common::register_mock_agent(&kernel, "burstwatcher", vec![]).await;

    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: "burstwatcher".to_string(),
            event_filter: "category:HardwareEvents".to_string(),
            payload_filter: None,
            throttle: Some("max:100/60s".to_string()),
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));

    let tasks_before = kernel.scheduler.list_tasks().await.len();

    const BURST: usize = 12;
    for i in 0..BURST {
        kernel
            .emit_event(
                EventType::HardwareAccessGranted,
                EventSource::HardwareAbstractionLayer,
                EventSeverity::Info,
                serde_json::json!({
                    "device_id": format!("network:veth{i}"),
                    "approved_by": "operator",
                }),
                0,
            )
            .await;
    }

    // Wait out the window plus slack, then confirm the batch produced one task.
    let mut batched: Option<agentos_types::AgentTask> = None;
    for _ in 0..75 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        for s in kernel
            .scheduler
            .list_tasks()
            .await
            .iter()
            .filter(|s| s.agent_id == agent_id)
        {
            if let Some(task) = kernel.scheduler.get_task(&s.id).await {
                if task
                    .trigger_source
                    .as_ref()
                    .map(|ts| ts.event_type == EventType::HardwareAccessGranted)
                    .unwrap_or(false)
                {
                    batched = Some(task);
                    break;
                }
            }
        }
        if batched.is_some() {
            break;
        }
    }
    let task = batched.expect("burst did not produce a reaction task within 3s");

    assert_eq!(
        kernel.scheduler.list_tasks().await.len(),
        tasks_before + 1,
        "a {BURST}-event burst must spawn exactly one reaction task"
    );
    assert!(
        task.original_prompt.contains("[EVENT BATCH]")
            && task
                .original_prompt
                .contains(&format!("HardwareAccessGranted x{BURST}")),
        "batched task should carry the digest; got: {}",
        task.original_prompt
    );

    // Every event is still individually recorded in the agent's inbox.
    let inbox_entries = kernel
        .agent_inbox
        .list(agent_id, false, 50)
        .await
        .expect("list inbox");
    assert_eq!(
        inbox_entries
            .iter()
            .filter(|e| e.title.contains("HardwareAccessGranted"))
            .count(),
        BURST,
        "the digest summarizes, the inbox keeps every payload"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// Attach a backend that takes `delay` to answer, so a reaction task for this
/// agent stays observably in flight while the next events arrive.
async fn attach_slow_backend(
    kernel: &agentos_kernel::Kernel,
    agent_id: agentos_types::AgentID,
    delay: Duration,
) {
    let host = spawn_ollama_stub_with_delay(delay).await;
    kernel.active_llms.write().await.insert(
        agent_id,
        std::sync::Arc::new(agentos_llm::OllamaCore::new(&host, "mock-model")),
    );
}

/// Subscribe `agent` to `category:HardwareEvents` with a throttle wide enough
/// for a burst.
async fn subscribe_hardware(client: &mut agentos_bus::BusClient, agent: &str) {
    let resp = client
        .send_command(KernelCommand::EventSubscribe {
            agent_name: agent.to_string(),
            event_filter: "category:HardwareEvents".to_string(),
            payload_filter: None,
            throttle: Some("max:100/60s".to_string()),
            priority: None,
        })
        .await
        .expect("send EventSubscribe");
    assert!(matches!(resp, KernelResponse::EventSubscriptionId(_)));
}

async fn emit_hardware_events(kernel: &agentos_kernel::Kernel, count: usize, tag: &str) {
    for i in 0..count {
        kernel
            .emit_event(
                EventType::HardwareAccessGranted,
                EventSource::HardwareAbstractionLayer,
                EventSeverity::Info,
                serde_json::json!({ "device_id": format!("{tag}:veth{i}") }),
                0,
            )
            .await;
    }
}

/// Every reaction task this agent has, newest state included.
async fn reaction_tasks(
    kernel: &agentos_kernel::Kernel,
    agent_id: agentos_types::AgentID,
) -> Vec<agentos_types::AgentTask> {
    let mut out = Vec::new();
    for s in kernel.scheduler.list_tasks().await {
        if s.agent_id != agent_id {
            continue;
        }
        if let Some(task) = kernel.scheduler.get_task(&s.id).await {
            if task.trigger_source.is_some() {
                out.push(task);
            }
        }
    }
    out
}

/// Poll until the agent has `want` reaction tasks, or give up after `tries`.
async fn wait_for_reaction_tasks(
    kernel: &agentos_kernel::Kernel,
    agent_id: agentos_types::AgentID,
    want: usize,
    tries: usize,
) -> Vec<agentos_types::AgentTask> {
    let mut tasks = Vec::new();
    for _ in 0..tries {
        tasks = reaction_tasks(kernel, agent_id).await;
        if tasks.len() >= want {
            return tasks;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tasks
}

/// At most ONE reaction task per agent may be in flight: events arriving while
/// one runs coalesce behind it and flush as a single digest once it finishes.
/// Without the gate a storm spawns a task per event on an agent that is already
/// busy — the 2026-08-31 shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn in_flight_reaction_task_gates_the_next_batch() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel_with(|cfg| {
        cfg.kernel.events.reaction_batch_window_secs = 1;
    })
    .await;

    let agent_id = common::register_mock_agent(&kernel, "gatewatcher", vec![]).await;
    attach_slow_backend(&kernel, agent_id, Duration::from_secs(6)).await;
    subscribe_hardware(&mut client, "gatewatcher").await;

    // One event → one reaction task, which then blocks on its slow backend.
    emit_hardware_events(&kernel, 1, "first").await;
    let first = wait_for_reaction_tasks(&kernel, agent_id, 1, 60).await;
    assert_eq!(
        first.len(),
        1,
        "the first event should have produced one reaction task"
    );
    let first_id = first[0].id;

    // Three more while it runs: they must queue behind it, not stack tasks.
    emit_hardware_events(&kernel, 3, "held").await;
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let live = reaction_tasks(&kernel, agent_id).await;
        // Stop asserting once the first task has actually finished — after that
        // a second task is expected, not a violation.
        if !matches!(
            kernel.scheduler.get_task(&first_id).await.map(|t| t.state),
            Some(agentos_types::TaskState::Queued)
                | Some(agentos_types::TaskState::Running)
                | Some(agentos_types::TaskState::Waiting)
        ) {
            break;
        }
        assert_eq!(
            live.len(),
            1,
            "no second reaction task may spawn while one is in flight"
        );
    }

    // Once the first task reaches a terminal state the drain flushes the batch
    // as exactly one more task, carrying all three events.
    let tasks = wait_for_reaction_tasks(&kernel, agent_id, 2, 200).await;
    assert_eq!(
        tasks.len(),
        2,
        "the held batch must flush as exactly one further task"
    );
    let batched = tasks
        .iter()
        .find(|t| t.id != first_id)
        .expect("second reaction task");
    assert!(
        batched.original_prompt.contains("HardwareAccessGranted x3"),
        "the flushed task must carry all three held events; got: {}",
        batched.original_prompt
    );
    // ...and the digest must keep the standard trigger framing, not arrive bare.
    for section in ["[SYSTEM CONTEXT]", "[EVENT BATCH]", "[CURRENT OS STATE]"] {
        assert!(
            batched.original_prompt.contains(section),
            "batched prompt is missing {section}; got: {}",
            batched.original_prompt
        );
    }

    kernel.shutdown();
    handle.await.unwrap();
}

/// `task cancel` does NOT call `drain_reactions_after_task` — nor do the
/// timeout checker, denied escalations or scheduler queue-cap rejections. The
/// in-flight slot must therefore heal itself by asking the scheduler whether
/// the recorded task is still live; otherwise one cancel wedges every future
/// reaction for that agent until the 2h stale backstop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn cancelled_reaction_task_does_not_wedge_the_batch() {
    let (kernel, mut client, _tmp, handle) = common::setup_kernel_with(|cfg| {
        cfg.kernel.events.reaction_batch_window_secs = 1;
    })
    .await;

    let agent_id = common::register_mock_agent(&kernel, "wedgewatcher", vec![]).await;
    // Long enough that the task cannot reach a terminal path on its own — the
    // only way the batch flushes is the liveness check.
    attach_slow_backend(&kernel, agent_id, Duration::from_secs(120)).await;
    subscribe_hardware(&mut client, "wedgewatcher").await;

    emit_hardware_events(&kernel, 1, "first").await;
    let first = wait_for_reaction_tasks(&kernel, agent_id, 1, 60).await;
    assert_eq!(first.len(), 1, "expected one reaction task to be in flight");
    let first_id = first[0].id;

    // Two more events pile up behind the in-flight task. Wait past the 1 s
    // window so their one-shot timer has already fired and been HELD — from
    // here only the held-path re-arm can ever flush them, which is exactly
    // the strand this test guards.
    emit_hardware_events(&kernel, 2, "behind").await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    // ...then the operator cancels it. This path clears no batcher state.
    let resp = client
        .send_command(KernelCommand::CancelTask { task_id: first_id })
        .await
        .expect("send CancelTask");
    assert!(
        matches!(resp, KernelResponse::Success { .. }),
        "cancel should succeed, got: {resp:?}"
    );

    let tasks = wait_for_reaction_tasks(&kernel, agent_id, 2, 150).await;
    assert_eq!(
        tasks.len(),
        2,
        "the batch held behind a cancelled reaction task must still flush"
    );
    let batched = tasks
        .iter()
        .find(|t| t.id != first_id)
        .expect("second reaction task");
    assert!(
        batched.original_prompt.contains("HardwareAccessGranted x2"),
        "the flushed task must carry both held events; got: {}",
        batched.original_prompt
    );

    kernel.shutdown();
    handle.await.unwrap();
}
