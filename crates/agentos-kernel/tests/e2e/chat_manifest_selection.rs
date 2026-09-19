//! Tests for `Kernel::build_chat_tool_manifests` — the dynamic chat tool
//! manifest selector that lets the LLM see schemas for MCP/extra tools the
//! agent has previously used (per-session and cross-session), without
//! re-running discovery every turn.

use crate::common;
use agentos_kernel::kernel::ChatTurnScope;
use agentos_types::tool::{ToolCapabilities, ToolInfo, ToolOutputs, ToolSchema};
use agentos_types::{ToolManifest, ToolSandbox, TrustTier};
use serial_test::serial;
use std::collections::HashSet;

fn make_extra_tool_manifest(name: &str) -> ToolManifest {
    ToolManifest {
        manifest: ToolInfo {
            category: None,
            search_hints: vec![],
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: format!("Test extra tool {name}"),
            author: "test".to_string(),
            checksum: None,
            author_pubkey: None,
            signature: None,
            trust_tier: TrustTier::Core,
            tags: None,
            capability_tags: vec![],
            group: String::new(),
        },
        capabilities_required: ToolCapabilities {
            permissions: vec![],
        },
        capabilities_provided: ToolOutputs { outputs: vec![] },
        intent_schema: ToolSchema {
            input: "None".to_string(),
            output: "None".to_string(),
        },
        payload_schema: None,
        examples: vec![],
        sandbox: ToolSandbox {
            network: false,
            fs_write: false,
            gpu: false,
            max_memory_mb: 64,
            max_cpu_ms: 1000,
            syscalls: vec![],
            weight: None,
        },
        executor: Default::default(),
        fallbacks: vec![],
        risk_class: Default::default(),
        risk_class_by_action: Default::default(),
        usage_hints: None,
        tags: vec![],
    }
}

fn names(manifests: &[ToolManifest]) -> HashSet<String> {
    manifests.iter().map(|m| m.manifest.name.clone()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn extra_tools_excluded_when_no_history() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "selector-agent", vec![]).await;

    {
        let mut reg = kernel.tool_registry.write().await;
        reg.register(make_extra_tool_manifest("gmail_send"))
            .expect("register");
    }

    let manifests = kernel
        .build_chat_tool_manifests(&agent_id, None, ChatTurnScope::Full)
        .await;
    let n = names(&manifests);
    assert!(
        !n.contains("gmail_send"),
        "extra tool must not be in chat manifest list without any usage history"
    );
    // Defaults still present.
    assert!(n.contains("file-reader"));
    assert!(n.contains("agent-manual"));

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn session_recent_tool_promoted() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "selector-agent", vec![]).await;

    {
        let mut reg = kernel.tool_registry.write().await;
        reg.register(make_extra_tool_manifest("gmail_send"))
            .expect("register");
    }

    // Simulate the previous turn having executed gmail_send in this session.
    // chat_session_dedup is the canonical record of what was actually run.
    let session_id = "test-session-promote".to_string();
    {
        let mut guard = kernel.chat_session_dedup.write().await;
        let now = std::time::Instant::now();
        let mut inner = std::collections::HashMap::new();
        inner.insert(
            ("gmail_send".to_string(), "{}".to_string()),
            (now, serde_json::json!({"ok": true})),
        );
        guard.insert(session_id.clone(), (now, inner));
    }

    let manifests = kernel
        .build_chat_tool_manifests(&agent_id, Some(&session_id), ChatTurnScope::Full)
        .await;
    assert!(
        names(&manifests).contains("gmail_send"),
        "session-recent tool must be promoted into the chat manifest list"
    );

    // Without the session id, the tool is gone again — proves the promotion is session-scoped.
    let manifests_no_sess = kernel
        .build_chat_tool_manifests(&agent_id, None, ChatTurnScope::Full)
        .await;
    assert!(
        !names(&manifests_no_sess).contains("gmail_send"),
        "tool must not leak across sessions when there is no cross-session usage rank yet"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn cross_session_usage_rank_promotes_tool() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "selector-agent", vec![]).await;

    {
        let mut reg = kernel.tool_registry.write().await;
        reg.register(make_extra_tool_manifest("gmail_send"))
            .expect("register");
    }

    // Cross-session signal: tool_usage_store is persistent and fed by both
    // the chat path and task_executor.
    kernel
        .tool_usage
        .record(&agent_id.to_string(), "gmail_send")
        .await;
    kernel
        .tool_usage
        .record(&agent_id.to_string(), "gmail_send")
        .await;

    let manifests = kernel
        .build_chat_tool_manifests(&agent_id, None, ChatTurnScope::Full)
        .await;
    assert!(
        names(&manifests).contains("gmail_send"),
        "cross-session usage-ranked tool must be promoted into the chat manifest list"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn meta_tool_names_never_promoted_as_extras() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "selector-agent", vec![]).await;

    // `agent-manual` is a meta tool AND in the chat defaults — so it will always
    // appear. To test the "meta tools are filtered from extras" guard we use a
    // non-default meta name and ensure it doesn't sneak in via usage rank.
    // `tool-detail` is in META_TOOL_NAMES but NOT in CHAT_DEFAULT_TOOL_NAMES.
    kernel
        .tool_usage
        .record(&agent_id.to_string(), "tool-detail")
        .await;

    let manifests = kernel
        .build_chat_tool_manifests(&agent_id, None, ChatTurnScope::Full)
        .await;
    let n = names(&manifests);
    // `tool-detail` is not even registered, but the assertion proves the guard
    // prevents the name from being treated as an extra-budget candidate.
    assert!(
        !n.contains("tool-detail"),
        "meta tool names must never consume the extras budget"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn extras_budget_is_capped() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "selector-agent", vec![]).await;

    // Register and rank 40 extra tools — well over the cap of 25.
    let extras: Vec<String> = (0..40).map(|i| format!("extra_{i}")).collect();
    {
        let mut reg = kernel.tool_registry.write().await;
        for name in &extras {
            reg.register(make_extra_tool_manifest(name)).expect("reg");
        }
    }
    for name in &extras {
        kernel.tool_usage.record(&agent_id.to_string(), name).await;
    }

    let manifests = kernel
        .build_chat_tool_manifests(&agent_id, None, ChatTurnScope::Full)
        .await;
    let n = names(&manifests);
    let promoted: Vec<&String> = extras.iter().filter(|name| n.contains(*name)).collect();
    assert!(
        promoted.len() <= 25,
        "extras budget should cap promotions at 25, got {}",
        promoted.len()
    );
    assert!(
        !promoted.is_empty(),
        "at least some extras should be promoted from a populated usage rank"
    );

    kernel.shutdown();
    handle.await.unwrap();
}

/// A conversation turn must not be offered the out-of-band egress family.
///
/// Driven through the session-recent promotion path, because that is how the
/// tool actually reached the model on 2026-09-09: `agent-message` is not a chat
/// default, it was promoted because the agent had used it before. The scope
/// filter therefore has to beat promotion, not just the default allowlist.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn convo_scope_withholds_out_of_band_tools() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "selector-agent", vec![]).await;

    {
        let mut reg = kernel.tool_registry.write().await;
        reg.register(make_extra_tool_manifest("agent-message"))
            .expect("register");
    }

    // Previous turn used agent-message in this session — the promotion path.
    let session_id = "test-session-convo-scope".to_string();
    {
        let mut guard = kernel.chat_session_dedup.write().await;
        let now = std::time::Instant::now();
        let mut inner = std::collections::HashMap::new();
        inner.insert(
            ("agent-message".to_string(), "{}".to_string()),
            (now, serde_json::json!({"ok": true})),
        );
        guard.insert(session_id.clone(), (now, inner));
    }

    let full = names(
        &kernel
            .build_chat_tool_manifests(&agent_id, Some(&session_id), ChatTurnScope::Full)
            .await,
    );
    assert!(
        full.contains("agent-message"),
        "session-recent promotion must offer agent-message on an ordinary turn"
    );

    let convo = names(
        &kernel
            .build_chat_tool_manifests(&agent_id, Some(&session_id), ChatTurnScope::ConvoTurn)
            .await,
    );
    // Assert against the whole list, not just the one name driven above: a
    // future entry added to the const must be honoured by the filter too.
    for withheld in agentos_kernel::kernel::CONVO_WITHHELD_TOOL_NAMES {
        assert!(
            !convo.contains(*withheld),
            "a convo turn must not be offered '{withheld}'"
        );
    }
    // Reading and reasoning tools are deliberately untouched — a convo turn may
    // look things up, it just may not reach out of band.
    assert!(convo.contains("file-reader"));
    assert!(convo.contains("agent-manual"));

    kernel.shutdown();
    handle.await.unwrap();
}

/// Chat turns ship a working set, not the whole candidate set. "hey" in a fresh
/// panel session cost 17.7k tokens on 2026-09-17 because all ~70 chat defaults
/// went natively; the rest must sit in the pool, reachable via discovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_working_set_partitions_candidates() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "selector-agent", vec![]).await;

    let candidates = kernel
        .build_chat_tool_manifests(&agent_id, None, ChatTurnScope::Full)
        .await;
    let all = names(&candidates);
    let (native, pool) = kernel
        .chat_working_set(&agent_id, None, "hey", candidates)
        .await;
    let native_names = names(&native);
    let pool_names: HashSet<String> = pool.keys().cloned().collect();

    assert!(
        native.len() <= 21,
        "chat native array must be a working set, got {}",
        native.len()
    );
    assert!(
        !pool.is_empty(),
        "the rest of the candidates must be deferred"
    );
    assert!(native_names.is_disjoint(&pool_names));
    assert_eq!(
        native_names
            .union(&pool_names)
            .cloned()
            .collect::<HashSet<_>>(),
        all,
        "every candidate lands on exactly one side"
    );
    for n in ["search-tools", "describe-tool"] {
        assert!(
            native_names.contains(n),
            "{n} must stay native for discovery"
        );
    }

    kernel.shutdown();
    handle.await.unwrap();
}

/// A tool the model already ran this session is pinned native, so a follow-up
/// ("do that again") doesn't need a search hop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_working_set_pins_session_recent_tools() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let agent_id = common::register_mock_agent(&kernel, "selector-agent", vec![]).await;

    let session_id = "test-session-working-set".to_string();
    {
        let mut guard = kernel.chat_session_dedup.write().await;
        let now = std::time::Instant::now();
        let mut inner = std::collections::HashMap::new();
        inner.insert(
            ("file-diff".to_string(), "{}".to_string()),
            (now, serde_json::json!({"ok": true})),
        );
        guard.insert(session_id.clone(), (now, inner));
    }

    let candidates = kernel
        .build_chat_tool_manifests(&agent_id, Some(&session_id), ChatTurnScope::Full)
        .await;
    assert!(names(&candidates).contains("file-diff"));
    let (native, pool) = kernel
        .chat_working_set(&agent_id, Some(&session_id), "hey", candidates)
        .await;
    assert!(names(&native).contains("file-diff"));
    assert!(!pool.contains_key("file-diff"));

    // Contrast: without the session, "hey" does not retrieve file-diff — so the
    // assertion above proves the pin, not a lucky T1 hit.
    let candidates = kernel
        .build_chat_tool_manifests(&agent_id, None, ChatTurnScope::Full)
        .await;
    let (native, pool) = kernel
        .chat_working_set(&agent_id, None, "hey", candidates)
        .await;
    assert!(!names(&native).contains("file-diff"));
    assert!(pool.contains_key("file-diff"));

    kernel.shutdown();
    handle.await.unwrap();
}
