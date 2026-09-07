//! Integration tests for the KernelService trait implementation.
//!
//! Each test boots a real kernel into a temp directory, calls KernelService
//! methods directly on the `Kernel`, and asserts expected behaviour.

use agentos_api::error::ApiError;
use agentos_api::types::{
    AttachMcpRequest, AuditFilter, ConnectChannelRequest, CreateChatSessionRequest,
    GrantWorkspaceRequest, SavePipelineRequest, StoreCredentialRequest, TaskFilter,
    UpdateChannelRequest,
};
use agentos_api::KernelService;
use agentos_kernel::config::{
    AuditSettings, BusSettings, HealthMonitorConfig, KernelConfig, KernelSettings, LlmSettings,
    MemorySettings, OllamaSettings, PreflightConfig, SecretsSettings, ToolsSettings,
};
use agentos_kernel::Kernel;
use agentos_types::TaskID;
use agentos_vault::ZeroizingString;
use serial_test::serial;
use std::sync::Arc;

// ── Test helpers ─────────────────────────────────────────────────────────────

fn shared_model_cache_dir() -> String {
    let cache_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-model-cache");
    std::fs::create_dir_all(&cache_dir).expect("create shared model cache dir");
    cache_dir.to_string_lossy().to_string()
}

fn create_test_config(temp_dir: &tempfile::TempDir) -> KernelConfig {
    KernelConfig {
        kernel: KernelSettings {
            max_concurrent_tasks: 4,
            default_task_timeout_secs: 60,
            context_window_max_entries: 100,
            context_window_token_budget: 0,
            state_db_path: temp_dir
                .path()
                .join("kernel_state.db")
                .to_string_lossy()
                .to_string(),
            task_limits: Default::default(),
            tool_calls: Default::default(),
            tool_execution: Default::default(),
            autonomous_mode: Default::default(),
            health_port: 0,
            health_bind: "127.0.0.1".to_string(),
            per_agent_rate_limit: 0,
            events: Default::default(),
            sandbox_policy: Default::default(),
            max_concurrent_sandbox_children: 4,
            context_compaction: Default::default(),
            max_queued_per_agent: 500,
            boot_replay_max_age_hours: 24,
            task_retention_days: 7,
            failure_streak_limit: 25,
            failure_streak_fast_ms: 5_000,
        },
        routing: Default::default(),
        secrets: SecretsSettings {
            vault_path: temp_dir
                .path()
                .join("vault/secrets.db")
                .to_string_lossy()
                .to_string(),
        },
        audit: AuditSettings {
            log_path: temp_dir
                .path()
                .join("data/audit.db")
                .to_string_lossy()
                .to_string(),
            max_audit_entries: 0,
            verify_last_n_entries: 0,
        },
        tools: ToolsSettings {
            core_tools_dir: temp_dir
                .path()
                .join("tools/core")
                .to_string_lossy()
                .to_string(),
            user_tools_dir: temp_dir
                .path()
                .join("tools/user")
                .to_string_lossy()
                .to_string(),
            data_dir: temp_dir.path().join("data").to_string_lossy().to_string(),
            crl_path: None,
            workspace: agentos_kernel::config::WorkspaceConfig::default(),
            host_package: agentos_kernel::config::HostPackageSettings::default(),
            discovery: Default::default(),
        },
        bus: BusSettings {
            socket_path: temp_dir
                .path()
                .join("agentos.sock")
                .to_string_lossy()
                .to_string(),
            tls: None,
        },
        ollama: OllamaSettings {
            host: "http://localhost:11434".to_string(),
            default_model: "llama3.2".to_string(),
            request_timeout_secs: 300,
        },
        llm: LlmSettings::default(),
        memory: MemorySettings {
            model_cache_dir: shared_model_cache_dir(),
            extraction: Default::default(),
            consolidation: Default::default(),
            context: Default::default(),
            disable_embedder: true,
            embedder_init_timeout_secs: 120,
            retention_days: 0,
            lifecycle: Default::default(),
            // No auxiliary review inference in API service tests.
            background_review: agentos_kernel::config::BackgroundReviewConfig {
                enabled: false,
                ..Default::default()
            },
        },
        context_budget: Default::default(),
        context: Default::default(),
        health_monitor: HealthMonitorConfig::default(),
        preflight: PreflightConfig::default(),
        logging: Default::default(),
        notifications: Default::default(),
        mcp: Default::default(),
        registry: Default::default(),
        scratchpad: Default::default(),
        skills: Default::default(),
        otel: agentos_kernel::config::OtelConfig::default(),
        approval: Default::default(),
        api: Default::default(),
        web: Default::default(),
        chat: Default::default(),
        user_adaptation: Default::default(),
        env: Default::default(),
        gateway: Default::default(),
        scheduler: Default::default(),
        transcription: Default::default(),
        agent_heartbeat: Default::default(),
        agent_budget: Default::default(),
        hal: Default::default(),
        security: Default::default(),
        user_profile: Default::default(),
        personalization: Default::default(),
    }
}

/// Boot a kernel into a fresh temp directory. Returns `(kernel, temp_dir)`.
/// Keep `temp_dir` alive for the test duration.
async fn boot_test_kernel() -> (Arc<Kernel>, tempfile::TempDir) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config = create_test_config(&temp_dir);
    let config_path = temp_dir.path().join("config.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    std::fs::create_dir_all(temp_dir.path().join("data")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("vault")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/core")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/user")).unwrap();

    let kernel = Arc::new(
        Kernel::boot(
            &config_path,
            &ZeroizingString::new("test-passphrase".to_string()),
        )
        .await
        .unwrap(),
    );
    kernel.wire_inbound_chat_bridge();

    (kernel, temp_dir)
}

/// `POST /pipelines` must not silently replace an existing pipeline.
///
/// The store writes `INSERT OR REPLACE`, so before the guard a "new" pipeline
/// named after an existing one overwrote a production definition and still
/// reported success. The version also has to come from the definition — it used
/// to be hardcoded to "1.0.0", downgrading anything that declared otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn save_pipeline_rejects_a_name_collision_unless_overwrite() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let definition = |version: &str, step: &str| {
        serde_json::json!({
            "name": "nightly-report",
            "version": version,
            "steps": [{ "id": step, "agent": "alpha", "task": "summarize" }],
        })
    };

    kernel
        .save_pipeline(SavePipelineRequest {
            name: "nightly-report".to_string(),
            definition: definition("2.3.0", "first"),
            overwrite: false,
        })
        .await
        .expect("first save creates the pipeline");

    // Same name, no overwrite → refused, and the stored definition is untouched.
    let err = kernel
        .save_pipeline(SavePipelineRequest {
            name: "nightly-report".to_string(),
            definition: definition("9.9.9", "second"),
            overwrite: false,
        })
        .await
        .expect_err("a colliding name must not overwrite");
    assert!(
        matches!(err, agentos_api::error::ApiError::Conflict(_)),
        "expected Conflict, got {err:?}"
    );

    let listed = kernel.list_pipelines().await.expect("list");
    assert_eq!(listed.len(), 1);
    let stored = kernel
        .get_pipeline_definition("nightly-report")
        .await
        .expect("read back the definition");
    assert_eq!(
        stored["version"], "2.3.0",
        "the refused save must not have touched the stored definition"
    );
    assert_eq!(stored["steps"][0]["id"], "first");

    // Explicit overwrite replaces it.
    kernel
        .save_pipeline(SavePipelineRequest {
            name: "nightly-report".to_string(),
            definition: definition("9.9.9", "second"),
            overwrite: true,
        })
        .await
        .expect("overwrite replaces");

    let listed = kernel.list_pipelines().await.expect("list after overwrite");
    assert_eq!(listed.len(), 1, "overwrite replaces, never duplicates");
    let stored = kernel
        .get_pipeline_definition("nightly-report")
        .await
        .expect("read back after overwrite");
    assert_eq!(stored["version"], "9.9.9");
    assert_eq!(stored["steps"][0]["id"], "second");
}

/// Importing YAML must not silently replace a pipeline that shares its name, and
/// a definition the engine cannot parse must be refused rather than stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn import_pipeline_refuses_a_collision_and_invalid_yaml() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let yaml = |step: &str| {
        format!("name: imported\nversion: 1.0.0\nsteps:\n  - id: {step}\n    agent: alpha\n    task: summarize\n")
    };

    kernel
        .import_pipeline(yaml("first"))
        .await
        .expect("first import creates the pipeline");

    let err = kernel
        .import_pipeline(yaml("second"))
        .await
        .expect_err("an import must not overwrite an existing name");
    assert!(
        matches!(err, agentos_api::error::ApiError::Conflict(_)),
        "expected Conflict, got {err:?}"
    );

    let stored = kernel
        .get_pipeline_definition("imported")
        .await
        .expect("read back");
    assert_eq!(stored["steps"][0]["id"], "first", "original untouched");

    // A document the engine could never run is refused, not stored as a blob that
    // only fails at run time.
    let err = kernel
        .save_pipeline(SavePipelineRequest {
            name: "broken".to_string(),
            definition: serde_json::json!({ "name": "broken", "steps": "not-a-list" }),
            overwrite: false,
        })
        .await
        .expect_err("an unparseable definition must be refused");
    assert!(
        matches!(err, agentos_api::error::ApiError::BadRequest(_)),
        "expected BadRequest, got {err:?}"
    );
    assert!(kernel
        .list_pipelines()
        .await
        .unwrap()
        .iter()
        .all(|p| p.name != "broken"));
}

/// A distinct name is unaffected by the collision guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn save_pipeline_allows_distinct_names() {
    let (kernel, _tmp) = boot_test_kernel().await;
    for name in ["one", "two"] {
        kernel
            .save_pipeline(SavePipelineRequest {
                name: name.to_string(),
                // `version` is required by `PipelineDefinition`; the REST path now
                // parses before storing, matching what the CLI has always done.
                definition: serde_json::json!({ "name": name, "version": "1.0.0", "steps": [] }),
                overwrite: false,
            })
            .await
            .unwrap_or_else(|e| panic!("save {name}: {e:?}"));
    }
    assert_eq!(kernel.list_pipelines().await.unwrap().len(), 2);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// list_agents returns an empty vec on a freshly booted kernel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_list_agents_empty_on_fresh_kernel() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let agents = kernel.list_agents().await.expect("list_agents");
    assert!(
        agents.is_empty(),
        "expected no agents on fresh kernel, got {}",
        agents.len()
    );
    kernel.shutdown();
}

/// list_tasks with a default filter returns empty on a fresh kernel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_list_tasks_empty_on_fresh_kernel() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let (tasks, total) = kernel
        .list_tasks(TaskFilter::default())
        .await
        .expect("list_tasks");
    assert!(tasks.is_empty(), "expected no tasks, got {}", tasks.len());
    assert_eq!(total, 0, "expected total=0");
    kernel.shutdown();
}

/// get_status returns sane initial values (0 agents, uptime >= 0, tool_count >= 0).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_get_status_returns_sane_initial_values() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let status = kernel.get_status().await.expect("get_status");
    assert_eq!(status.agent_count, 0, "expected 0 agents");
    assert_eq!(status.task_count, 0, "expected 0 tasks");
    // Core tools may or may not load in empty temp dir — just assert >= 0
    let _ = status.tool_count;
    assert!(
        status.uptime_secs < 30,
        "uptime should be <30s after boot, got {}",
        status.uptime_secs
    );
    kernel.shutdown();
}

/// get_uptime is less than 10 seconds immediately after boot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_get_uptime_short_after_boot() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let uptime = kernel.get_uptime().await;
    assert!(
        uptime.as_secs() < 10,
        "expected uptime <10s immediately after boot, got {:?}",
        uptime
    );
    kernel.shutdown();
}

/// get_dashboard_summary returns a composite with 0 agents and 0 tasks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_get_dashboard_summary_empty_kernel() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let summary = kernel
        .get_dashboard_summary()
        .await
        .expect("get_dashboard_summary");
    assert_eq!(summary.agent_count, 0, "expected 0 agents in dashboard");
    assert!(
        summary.online_agents.is_empty(),
        "expected empty online_agents"
    );
    assert_eq!(
        summary.task_counts.total, 0,
        "expected 0 tasks in dashboard"
    );
    assert_eq!(summary.task_counts.running, 0, "expected 0 running tasks");
    kernel.shutdown();
}

/// cancel_task with a random TaskID returns an error (task not found).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_cancel_nonexistent_task_returns_error() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let random_id = TaskID::new();
    let result = kernel.cancel_task(random_id).await;
    assert!(
        result.is_err(),
        "expected error when cancelling nonexistent task"
    );
    kernel.shutdown();
}

/// get_agent_detail for a nonexistent agent returns a NotFound error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_get_agent_detail_nonexistent_returns_not_found() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let result = kernel.get_agent_detail("no-such-agent").await;
    match result {
        Err(agentos_api::ApiError::NotFound(msg)) => {
            assert!(
                msg.contains("no-such-agent"),
                "NotFound message should mention agent name: {msg}"
            );
        }
        other => panic!("expected NotFound error, got: {other:?}"),
    }
    kernel.shutdown();
}

/// list_secrets returns without error on a fresh kernel (vault is empty).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_list_secrets_does_not_error() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let secrets = kernel.list_secrets().await.expect("list_secrets");
    // Fresh vault has no secrets.
    assert!(
        secrets.is_empty(),
        "expected no secrets in fresh vault, got {}",
        secrets.len()
    );
    kernel.shutdown();
}

/// query_audit with limit 5 returns without error.
/// The kernel logs at least a KernelStarted event at boot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_query_audit_returns_entries_after_boot() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let entries = kernel
        .query_audit(AuditFilter {
            limit: Some(5),
            ..Default::default()
        })
        .await
        .expect("query_audit");
    // At minimum a KernelStarted event is logged at boot.
    assert!(
        !entries.is_empty(),
        "expected at least one audit entry after boot"
    );
    kernel.shutdown();
}

/// get_cost_summary returns empty on a fresh kernel (no agents have run).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_get_cost_summary_empty_on_fresh_kernel() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let costs = kernel.get_cost_summary().await.expect("get_cost_summary");
    assert!(
        costs.is_empty(),
        "expected no cost entries on fresh kernel, got {}",
        costs.len()
    );
    kernel.shutdown();
}

/// get_unread_count returns 0 on a fresh kernel (no notifications sent).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_get_unread_count_zero_on_fresh_kernel() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let count = kernel.get_unread_count().await.expect("get_unread_count");
    assert_eq!(count, 0, "expected 0 unread notifications on fresh kernel");
    kernel.shutdown();
}

/// list_tools returns whatever core tools are registered; no error expected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_list_tools_no_error() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let result = kernel.list_tools().await;
    assert!(result.is_ok(), "list_tools should not error: {result:?}");
    kernel.shutdown();
}

// ── ApiError mapping tests (no kernel needed) ─────────────────────────────────

/// AgentOSError::TaskNotFound maps to ApiError::NotFound.
#[test]
fn test_api_error_from_task_not_found() {
    use agentos_api::ApiError;
    use agentos_types::AgentOSError;

    let err = AgentOSError::TaskNotFound(TaskID::new());
    let api_err = ApiError::from(err);
    assert!(
        matches!(api_err, ApiError::NotFound(_)),
        "expected NotFound, got: {api_err:?}"
    );
}

/// AgentOSError::AgentNotFound maps to ApiError::NotFound.
#[test]
fn test_api_error_from_agent_not_found() {
    use agentos_api::ApiError;
    use agentos_types::AgentOSError;

    let err = AgentOSError::AgentNotFound("test-agent".to_string());
    let api_err = ApiError::from(err);
    assert!(
        matches!(api_err, ApiError::NotFound(_)),
        "expected NotFound, got: {api_err:?}"
    );
}

/// AgentOSError::PermissionDenied maps to ApiError::Forbidden.
#[test]
fn test_api_error_from_permission_denied() {
    use agentos_api::ApiError;
    use agentos_types::AgentOSError;

    let err = AgentOSError::PermissionDenied {
        resource: "resource".to_string(),
        operation: "read".to_string(),
    };
    let api_err = ApiError::from(err);
    assert!(
        matches!(api_err, ApiError::Forbidden(_)),
        "expected Forbidden, got: {api_err:?}"
    );
}

/// AgentOSError::RateLimited maps to ApiError::RateLimited.
#[test]
fn test_api_error_from_rate_limited() {
    use agentos_api::ApiError;
    use agentos_types::AgentOSError;

    let err = AgentOSError::RateLimited {
        detail: "too many requests".to_string(),
    };
    let api_err = ApiError::from(err);
    assert!(
        matches!(api_err, ApiError::RateLimited(_)),
        "expected RateLimited, got: {api_err:?}"
    );
}

/// ApiError::NotFound status code is 404.
#[test]
fn test_api_error_not_found_status_code() {
    use agentos_api::ApiError;
    use axum::http::StatusCode;

    let err = ApiError::NotFound("something".to_string());
    assert_eq!(err.status_code(), StatusCode::NOT_FOUND);
}

/// ApiError::Internal status code is 500.
#[test]
fn test_api_error_internal_status_code() {
    use agentos_api::ApiError;
    use axum::http::StatusCode;

    let err = ApiError::Internal("oops".to_string());
    assert_eq!(err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// TaskFilter default has all fields as None (no filtering applied).
#[test]
fn test_task_filter_default_all_none() {
    let f = TaskFilter::default();
    assert!(f.status.is_none());
    assert!(f.agent_name.is_none());
    assert!(f.limit.is_none());
    assert!(f.offset.is_none());
}

/// AuditFilter default has all fields as None.
#[test]
fn test_audit_filter_default_all_none() {
    let f = AuditFilter::default();
    assert!(f.limit.is_none());
    assert!(f.severity.is_none());
    assert!(f.from.is_none());
    assert!(f.to.is_none());
}

// ── Control-plane auth (Phase 01) ──────────────────────────────────────────

/// Boot a kernel with `[api] operator_token` set, for login-credential tests.
async fn boot_kernel_with_operator_token(token: &str) -> (Arc<Kernel>, tempfile::TempDir) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let mut config = create_test_config(&temp_dir);
    config.api.operator_token = Some(token.to_string());
    let config_path = temp_dir.path().join("config.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    std::fs::create_dir_all(temp_dir.path().join("data")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("vault")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/core")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/user")).unwrap();

    let kernel = Arc::new(
        Kernel::boot(
            &config_path,
            &ZeroizingString::new("test-passphrase".to_string()),
        )
        .await
        .unwrap(),
    );
    (kernel, temp_dir)
}

/// With no `operator_token` configured, login is disabled (`NotConfigured`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn verify_operator_credential_not_configured_by_default() {
    let (kernel, _td) = boot_test_kernel().await;
    assert_eq!(
        kernel.verify_operator_credential("anything").await,
        agentos_api::service::CredentialCheck::NotConfigured
    );
}

/// A configured operator token accepts the exact credential and rejects others.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn verify_operator_credential_valid_and_invalid() {
    use agentos_api::service::CredentialCheck;
    let (kernel, _td) = boot_kernel_with_operator_token("s3cret-operator-token").await;
    assert_eq!(
        kernel
            .verify_operator_credential("s3cret-operator-token")
            .await,
        CredentialCheck::Valid
    );
    assert_eq!(
        kernel.verify_operator_credential("wrong").await,
        CredentialCheck::Invalid
    );
    assert_eq!(
        kernel.verify_operator_credential("").await,
        CredentialCheck::Invalid
    );
}

/// Folder access: grant → list → revoke, plus the three refusals the panel
/// relies on to render a status instead of a generic 500.
///
/// The path validation lives in `WorkspaceGrantStore`, so this also pins the
/// mapping from its error strings onto status codes — a silent change there
/// would otherwise surface as an `Internal` and look like a server bug.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn workspace_grant_round_trip_and_refusals() {
    let (kernel, tmp) = boot_test_kernel().await;
    let dir = tmp.path().join("shared-project");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.to_string_lossy().to_string();

    let grant = kernel
        .grant_workspace(
            GrantWorkspaceRequest {
                path: path.clone(),
                mode: Some("rwx".to_string()),
                agent_name: None,
            },
            "test",
        )
        .await
        .expect("global grant accepted");
    assert_eq!(grant.mode, "rwx");
    assert!(grant.agent_id.is_none(), "no agent scope means every agent");

    let listed = kernel.list_workspace_grants(None).await.unwrap();
    assert!(listed.iter().any(|g| g.path == path && g.mode == "rwx"));

    // Same (path, scope) twice is a conflict, not a second row.
    assert!(matches!(
        kernel
            .grant_workspace(
                GrantWorkspaceRequest {
                    path: path.clone(),
                    mode: Some("r".to_string()),
                    agent_name: None,
                },
                "test",
            )
            .await,
        Err(ApiError::Conflict(_))
    ));

    // A relative path never reaches the store.
    assert!(matches!(
        kernel
            .grant_workspace(
                GrantWorkspaceRequest {
                    path: "relative/path".to_string(),
                    mode: None,
                    agent_name: None,
                },
                "test",
            )
            .await,
        Err(ApiError::BadRequest(_))
    ));

    // An unknown agent is the caller's mistake, not a store failure.
    assert!(matches!(
        kernel
            .grant_workspace(
                GrantWorkspaceRequest {
                    path: path.clone(),
                    mode: None,
                    agent_name: Some("no-such-agent".to_string()),
                },
                "test",
            )
            .await,
        Err(ApiError::NotFound(_))
    ));

    // `..` never reaches the store's normalizer, which would have popped it and
    // stored a silently wider path than the caller — or the panel's confirmation
    // dialog — ever saw.
    assert!(matches!(
        kernel
            .grant_workspace(
                GrantWorkspaceRequest {
                    path: format!("{path}/sub/.."),
                    mode: None,
                    agent_name: None,
                },
                "test",
            )
            .await,
        Err(ApiError::BadRequest(_))
    ));

    // A forbidden system root goes through the store's validator, exercising the
    // branch of `workspace_cmd_err` that the relative-path case never reaches.
    assert!(matches!(
        kernel
            .grant_workspace(
                GrantWorkspaceRequest {
                    path: "/etc/agentos".to_string(),
                    mode: None,
                    agent_name: None,
                },
                "test",
            )
            .await,
        Err(ApiError::BadRequest(_))
    ));

    // An unparseable mode is the caller's mistake, not a store failure.
    assert!(matches!(
        kernel
            .grant_workspace(
                GrantWorkspaceRequest {
                    path: tmp.path().join("other").to_string_lossy().to_string(),
                    mode: Some("q".to_string()),
                    agent_name: None,
                },
                "test",
            )
            .await,
        Err(ApiError::BadRequest(_))
    ));

    assert_eq!(
        kernel
            .revoke_workspace(path.clone(), None, "test")
            .await
            .unwrap(),
        1
    );
    // Revoking twice reports nothing matched, so the handler can 404 instead of
    // telling the operator it removed a grant that was already gone.
    assert_eq!(
        kernel
            .revoke_workspace(path.clone(), None, "test")
            .await
            .unwrap(),
        0
    );
    assert!(!kernel
        .list_workspace_grants(None)
        .await
        .unwrap()
        .iter()
        .any(|g| g.path == path));
}

/// Put an agent in the registry and return its id. Inserts the profile directly
/// rather than going through `connect_agent`, which would demand a reachable LLM
/// endpoint; the workspace tests only need a name that resolves.
async fn register_workspace_test_agent(kernel: &Arc<Kernel>, name: &str) -> agentos_types::AgentID {
    let now = chrono::Utc::now();
    let profile = agentos_types::AgentProfile {
        id: agentos_types::AgentID::new(),
        name: name.to_string(),
        provider: agentos_types::LLMProvider::Ollama,
        model: "test-model".to_string(),
        status: agentos_types::AgentStatus::Online,
        permissions: agentos_types::PermissionSet::default(),
        roles: vec![],
        current_task: None,
        description: String::new(),
        created_at: now,
        last_active: now,
        public_key_hex: None,
        base_url: None,
        default_thinking_level: agentos_types::ThinkingLevel::default(),
        system_prompt: None,
        manually_offline: false,
    };
    kernel.agent_registry.write().await.register(profile)
}

/// Agent-scoped grants must not be reachable — or revocable — through the
/// global scope, and vice versa. This is the invariant that keeps "give this one
/// agent my project folder" from becoming "give every agent my project folder",
/// so it is asserted from both directions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn workspace_grant_scopes_do_not_leak_across_agents() {
    let (kernel, tmp) = boot_test_kernel().await;
    let alpha = register_workspace_test_agent(&kernel, "alpha").await;
    let beta = register_workspace_test_agent(&kernel, "beta").await;

    let mine = tmp.path().join("alpha-only");
    let shared = tmp.path().join("everyones");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    let mine_s = mine.to_string_lossy().to_string();
    let shared_s = shared.to_string_lossy().to_string();

    // Scoped by display name; the stored grant carries alpha's id.
    let scoped = kernel
        .grant_workspace(
            GrantWorkspaceRequest {
                path: mine_s.clone(),
                mode: Some("r".to_string()),
                agent_name: Some("alpha".to_string()),
            },
            "test",
        )
        .await
        .unwrap();
    assert_eq!(scoped.agent_id.as_deref(), Some(alpha.to_string().as_str()));
    // `mode: None` defaults to read+write.
    assert_eq!(
        kernel
            .grant_workspace(
                GrantWorkspaceRequest {
                    path: shared_s.clone(),
                    mode: None,
                    agent_name: None,
                },
                "test",
            )
            .await
            .unwrap()
            .mode,
        "rw"
    );

    // alpha sees its own grant plus the global one; beta sees only the global.
    let for_alpha = kernel
        .list_workspace_grants(Some("alpha".to_string()))
        .await
        .unwrap();
    assert!(for_alpha.iter().any(|g| g.path == mine_s));
    assert!(for_alpha.iter().any(|g| g.path == shared_s));
    let for_beta = kernel
        .list_workspace_grants(Some(beta.to_string()))
        .await
        .unwrap();
    assert!(!for_beta.iter().any(|g| g.path == mine_s));
    assert!(for_beta.iter().any(|g| g.path == shared_s));

    // A global revoke must not reach an agent-scoped grant...
    assert_eq!(
        kernel
            .revoke_workspace(mine_s.clone(), None, "test")
            .await
            .unwrap(),
        0
    );
    // ...nor may an agent-scoped revoke reach the global one.
    assert_eq!(
        kernel
            .revoke_workspace(shared_s.clone(), Some("alpha".to_string()), "test")
            .await
            .unwrap(),
        0
    );
    assert!(kernel
        .list_workspace_grants(Some("alpha".to_string()))
        .await
        .unwrap()
        .iter()
        .any(|g| g.path == mine_s));

    // An agent id that no longer resolves is a 404, not a grant written against
    // a UUID that `list_for_agent` can never match.
    assert!(matches!(
        kernel
            .grant_workspace(
                GrantWorkspaceRequest {
                    path: shared_s,
                    mode: None,
                    agent_name: Some(agentos_types::AgentID::new().to_string()),
                },
                "test",
            )
            .await,
        Err(ApiError::NotFound(_))
    ));
}

// ── HTTP-level integration: drive the real `build_router` end-to-end ────────

/// The folder-access endpoints are gated by `workspace:r` / `workspace:w`, and a
/// revoke that matched no active grant is a 404 rather than a cheerful 200.
/// Driven through the real router so the auth and permission middleware are in
/// the path — the service-level tests above bypass both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_workspace_grants_enforce_scopes() {
    let (kernel, tmp) = boot_test_kernel().await;
    let dir = tmp.path().join("scoped");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.to_string_lossy().to_string();

    let store = agentos_api::ApiKeyStore::new();
    let reader = store
        .create_key("reader".into(), vec!["workspace:r".into()], None)
        .await;
    let writer = store
        .create_key("writer".into(), vec!["workspace:rw".into()], None)
        .await;
    let svc: Arc<dyn agentos_api::KernelService> = kernel.clone();
    let app = agentos_api::build_router(
        svc,
        store,
        agentos_api::ws::broadcaster::WsBroadcaster::new(),
        "127.0.0.1:8080".parse().unwrap(),
        true,
        vec![],
        false,
    )
    .expect("router builds");

    let body = format!(r#"{{"path":"{path}","mode":"rw"}}"#);

    let (s, _) = send(&app, Method::GET, "/api/v1/workspace-grants", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Read scope lists but must not grant. `require_permission` matches the op
    // by substring, so a `:r` key slipping past the `:w` check is a live risk
    // worth pinning rather than assuming.
    let (s, _) = send(
        &app,
        Method::GET,
        "/api/v1/workspace-grants",
        Some(&reader),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(
        &app,
        Method::POST,
        "/api/v1/workspace-grants",
        Some(&reader),
        Some(&body),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Write scope grants — and the grant records the calling key, not the CLI,
    // so the audit trail can tell a remote grant from one typed at a terminal.
    let (s, created) = send(
        &app,
        Method::POST,
        "/api/v1/workspace-grants",
        Some(&writer),
        Some(&body),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(created["data"]["granted_by"], "writer");
    assert_eq!(created["data"]["source"], "api");

    // The temp path is plain ASCII with no reserved characters, so it needs no
    // percent-encoding to survive the query string.
    let uri = format!("/api/v1/workspace-grants?path={path}");
    let (s, _) = send(&app, Method::DELETE, &uri, Some(&reader), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&app, Method::DELETE, &uri, Some(&writer), None).await;
    assert_eq!(s, StatusCode::OK);
    // Nothing left to revoke — a stale row in the caller's list, not a success.
    let (s, _) = send(&app, Method::DELETE, &uri, Some(&writer), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

/// Build the production router around a real kernel + a fresh in-memory key store.
fn auth_router(kernel: &Arc<Kernel>, cors: Vec<String>, refresh: bool) -> axum::Router {
    let svc: Arc<dyn agentos_api::KernelService> = kernel.clone();
    let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
    agentos_api::build_router(
        svc,
        agentos_api::ApiKeyStore::new(),
        agentos_api::ws::broadcaster::WsBroadcaster::new(),
        addr,
        true,
        cors,
        refresh,
    )
    .expect("router builds")
}

/// Send a request through the router. Injects `ConnectInfo` so the rate-limit
/// governor's peer-IP key extractor works under `oneshot` (no real connection).
async fn send(
    app: &axum::Router,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(b) = bearer {
        builder = builder.header("authorization", format!("Bearer {b}"));
    }
    let req = match body {
        Some(json) => builder
            .header("content-type", "application/json")
            .body(Body::from(json.to_owned()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let mut req = req;
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:40000".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

/// Perform a real RFC 6455 handshake over TCP and return the HTTP status code.
/// `oneshot` can't exercise the upgrade route — the `WebSocketUpgrade`
/// extractor needs hyper's `OnUpgrade` connection machinery — so the WS leg of
/// the test talks to a genuinely served socket.
async fn ws_handshake_status(addr: std::net::SocketAddr, path_and_query: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "GET {path_and_query} HTTP/1.1\r\n\
         host: {addr}\r\n\
         connection: upgrade\r\n\
         upgrade: websocket\r\n\
         sec-websocket-version: 13\r\n\
         sec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).await.unwrap();
    let head = std::str::from_utf8(&buf[..n]).unwrap();
    head.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// A ticket minted via the protected `POST /ws/ticket` must redeem on the
/// public `GET /ws` upgrade — this pins the Extension-layering invariant that
/// both routes share ONE `WsTicketStore` (layer applied after every merge).
/// Also covers: unauthenticated mint, single-use redeem, credential-less 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_ws_ticket_mint_and_redeem() {
    let (kernel, _td) = boot_kernel_with_operator_token("op-token").await;
    let app = auth_router(&kernel, vec![], false);

    // Serve the SAME router on an ephemeral port for the WS handshake legs.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            served.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // Login for a bearer key (REST legs go through `oneshot`).
    let (s, body) = send(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(r#"{"credential":"op-token"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let key = body["data"]["api_key"].as_str().unwrap().to_string();

    // Mint requires auth.
    let (s, _) = send(&app, Method::POST, "/api/v1/ws/ticket", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Authed mint returns a seconds-lived ticket.
    let (s, body) = send(&app, Method::POST, "/api/v1/ws/ticket", Some(&key), None).await;
    assert_eq!(s, StatusCode::OK);
    let ticket = body["data"]["ticket"].as_str().unwrap().to_string();
    assert_eq!(body["data"]["expires_in"], 30);

    // Redeem on the PUBLIC upgrade route → 101 Switching Protocols. This pins
    // the shared-store invariant: the ticket minted through the protected
    // route must be visible to the public upgrade route.
    let path = format!("/api/v1/ws?ticket={ticket}");
    assert_eq!(ws_handshake_status(addr, &path).await, 101);

    // Single-use: the same ticket is consumed.
    assert_eq!(ws_handshake_status(addr, &path).await, 401);

    // No ticket and no token → 401 (fail-closed).
    assert_eq!(ws_handshake_status(addr, "/api/v1/ws").await, 401);

    // Legacy raw-key auth still works for script clients.
    let path = format!("/api/v1/ws?token={key}");
    assert_eq!(ws_handshake_status(addr, &path).await, 101);

    server.abort();
}

/// Full operator-login → use key → create/list/revoke key lifecycle over HTTP.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_auth_login_keys_lifecycle() {
    let (kernel, _td) = boot_kernel_with_operator_token("op-token").await;
    let app = auth_router(&kernel, vec!["http://localhost:5173".to_string()], true);

    // Wrong credential → 401.
    let (s, _) = send(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(r#"{"credential":"nope"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Correct credential → 200 with a one-time key.
    let (s, body) = send(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(r#"{"credential":"op-token"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let op_key = body["data"]["api_key"].as_str().unwrap().to_string();
    assert!(op_key.starts_with("agos_"));
    assert_eq!(body["data"]["scopes"][0], "*:rw");

    // The minted key authenticates a protected route.
    let (s, me) = send(&app, Method::GET, "/api/v1/auth/me", Some(&op_key), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["data"]["scopes"][0], "*:rw");

    // Create a scoped key.
    let (s, body) = send(
        &app,
        Method::POST,
        "/api/v1/keys",
        Some(&op_key),
        Some(r#"{"name":"ci","scopes":["agents:r"]}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let new_id = body["data"]["key_id"].as_str().unwrap().to_string();
    let new_key = body["data"]["api_key"].as_str().unwrap().to_string();

    // List shows metadata by id but never the raw key material.
    let (s, list) = send(&app, Method::GET, "/api/v1/keys", Some(&op_key), None).await;
    assert_eq!(s, StatusCode::OK);
    let list_str = serde_json::to_string(&list).unwrap();
    assert!(
        list_str.contains(&new_id),
        "list should reference the key id"
    );
    assert!(
        !list_str.contains(&new_key),
        "list must never leak raw key material"
    );

    // The new key works until revoked, then is rejected.
    let (s, _) = send(&app, Method::GET, "/api/v1/auth/me", Some(&new_key), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(
        &app,
        Method::DELETE,
        &format!("/api/v1/keys/{new_id}"),
        Some(&op_key),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(&app, Method::GET, "/api/v1/auth/me", Some(&new_key), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// Login is disabled (503) when no operator token is configured.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_login_disabled_without_operator_token() {
    let (kernel, _td) = boot_test_kernel().await;
    let app = auth_router(&kernel, vec![], false);
    let (s, _) = send(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(r#"{"credential":"anything"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

/// Unauthenticated access to a protected route is rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_protected_route_requires_bearer() {
    let (kernel, _td) = boot_test_kernel().await;
    let app = auth_router(&kernel, vec![], false);
    let (s, _) = send(&app, Method::GET, "/api/v1/keys", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// `POST /auth/refresh` is absent (404) when `[api] refresh_enabled` is false.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_refresh_absent_when_disabled() {
    let (kernel, _td) = boot_kernel_with_operator_token("op-token").await;
    let app = auth_router(&kernel, vec![], false);
    // Log in first to get a valid key, then attempt refresh.
    let (_, body) = send(
        &app,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(r#"{"credential":"op-token"}"#),
    )
    .await;
    let key = body["data"]["api_key"].as_str().unwrap().to_string();
    let (s, _) = send(&app, Method::POST, "/api/v1/auth/refresh", Some(&key), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// A CORS preflight from an allowed origin is reflected in the response headers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_cors_preflight_allows_configured_origin() {
    let (kernel, _td) = boot_test_kernel().await;
    let app = auth_router(&kernel, vec!["http://localhost:5173".to_string()], false);
    let mut req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/v1/agents")
        .header("origin", "http://localhost:5173")
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "authorization")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:40000".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let resp = app.oneshot(req).await.unwrap();
    let allow_origin = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(allow_origin, "http://localhost:5173");
}

// ── Conversational surface (Phase 02) ───────────────────────────────────────
// These exercise the store-backed CRUD/fork/export paths and the convo
// validation/lifecycle, all without LLM inference (deterministic).

fn new_session_req(agent: &str, first: &str) -> CreateChatSessionRequest {
    CreateChatSessionRequest {
        agent_name: agent.to_string(),
        title: None,
        first_message: Some(first.to_string()),
    }
}

/// A client that opens the chat lazily sends no `first_message`: the session is
/// created empty rather than 400-ing or seeding a blank user turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_session_without_first_message_starts_empty() {
    let (kernel, _tmp) = boot_test_kernel().await;

    for first in [None, Some("   ".to_string())] {
        let detail = kernel
            .create_chat_session(CreateChatSessionRequest {
                agent_name: "alpha".to_string(),
                title: None,
                first_message: first,
            })
            .await
            .expect("create session without a first message");
        assert!(detail.messages.is_empty(), "no placeholder turn");
    }

    let summaries = kernel.list_chat_sessions().await.expect("list sessions");
    assert_eq!(summaries.len(), 2);
    assert!(summaries.iter().all(|s| s.message_count == 0));
}

/// create → list reports message_count; messages carry `timestamp`; rename + delete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_session_crud_count_and_timestamp() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let detail = kernel
        .create_chat_session(new_session_req("alpha", "hello world"))
        .await
        .expect("create session");
    assert_eq!(detail.agent_name, "alpha");
    assert_eq!(detail.messages.len(), 1, "first_message persisted");

    // list → exactly one summary, message_count == 1 (the fix), preview present.
    let summaries = kernel.list_chat_sessions().await.expect("list sessions");
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].message_count, 1,
        "message_count must reflect rows"
    );
    assert_eq!(summaries[0].agent_name, "alpha");

    // messages expose the renamed `timestamp` field, non-empty.
    let msgs = kernel
        .get_chat_messages(&detail.id)
        .await
        .expect("messages");
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[0].content, "hello world");
    assert!(!msgs[0].timestamp.is_empty(), "timestamp populated");

    // rename then read back.
    kernel
        .rename_chat_session(&detail.id, Some("Renamed".into()))
        .await
        .expect("rename");
    let after = kernel.get_chat_session(&detail.id).await.expect("get");
    assert_eq!(after.title.as_deref(), Some("Renamed"));

    // delete → gone.
    kernel
        .delete_chat_session(&detail.id)
        .await
        .expect("delete");
    assert!(kernel.list_chat_sessions().await.unwrap().is_empty());
    kernel.shutdown();
}

/// fork copies the prefix history into a new session and leaves the source intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_fork_copies_prefix_history() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let src = kernel
        .create_chat_session(new_session_req("alpha", "seed message"))
        .await
        .expect("create");

    let fork_id = kernel.fork_chat_session(&src.id, None).await.expect("fork");
    assert_ne!(fork_id, src.id, "fork is a distinct session");

    let forked_msgs = kernel.get_chat_messages(&fork_id).await.expect("fork msgs");
    assert!(
        forked_msgs.iter().any(|m| m.content == "seed message"),
        "fork must copy prefix history"
    );
    // Source is untouched.
    assert!(kernel.get_chat_session(&src.id).await.is_ok());
    kernel.shutdown();
}

/// export returns the message text in both markdown and json forms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_export_contains_messages() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let s = kernel
        .create_chat_session(new_session_req("alpha", "exported line"))
        .await
        .expect("create");

    let (md_bytes, md_ct, md_name) = kernel
        .export_chat_session(&s.id, "markdown")
        .await
        .expect("md");
    assert!(!md_name.is_empty());
    assert!(md_ct.contains("markdown") || md_ct.contains("text"));
    assert!(String::from_utf8_lossy(&md_bytes).contains("exported line"));

    let (json_bytes, json_ct, _) = kernel
        .export_chat_session(&s.id, "json")
        .await
        .expect("json");
    assert!(json_ct.contains("json"));
    let parsed: serde_json::Value = serde_json::from_slice(&json_bytes).expect("valid json export");
    assert!(parsed.to_string().contains("exported line"));
    kernel.shutdown();
}

/// Agent-chat validates participant bounds (2–8), reports `running`, and 404s a
/// stop on an unknown convo.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn agent_chat_bounds_status_and_stop_404() {
    let (kernel, _tmp) = boot_test_kernel().await;

    // < 2 participants → 400.
    assert!(matches!(
        kernel
            .create_agent_chat("t".into(), vec!["solo".into()], 3)
            .await,
        Err(agentos_api::ApiError::BadRequest(_))
    ));
    // > 8 participants → 400.
    let many: Vec<String> = (0..9).map(|i| format!("a{i}")).collect();
    assert!(matches!(
        kernel.create_agent_chat("t".into(), many, 3).await,
        Err(agentos_api::ApiError::BadRequest(_))
    ));

    // Valid → created with status "running" (matches the store).
    let convo = kernel
        .create_agent_chat("t".into(), vec!["a".into(), "b".into()], 3)
        .await
        .expect("create convo");
    assert_eq!(convo.status, "running");

    // Stop a real convo succeeds; stop an unknown convo → NotFound.
    kernel.stop_agent_chat(&convo.id).await.expect("stop ok");
    assert!(matches!(
        kernel.stop_agent_chat("does-not-exist").await,
        Err(agentos_api::ApiError::NotFound(_))
    ));
    kernel.shutdown();
}

// ── Files & content surface (Phase 06) ──────────────────────────────────────

/// upload → list (+ tag filter) → get → download (verbatim bytes + allowed MIME)
/// → delete (then 404).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn file_upload_list_download_roundtrip() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let owner = "alice";
    let png = vec![0x89u8, b'P', b'N', b'G', 1, 2, 3, 4];

    let meta = kernel
        .upload_file(
            owner,
            "pic.png",
            "image/png",
            "global",
            &["holiday".into()],
            png.clone(),
        )
        .await
        .expect("upload");
    assert_eq!(meta.mime, "image/png");
    assert_eq!(meta.size, png.len() as u64);

    // Tag filter: matching tag returns it, non-matching returns empty.
    let hit = kernel
        .list_files(owner, None, Some("holiday"), None)
        .await
        .unwrap();
    assert!(hit.iter().any(|f| f.id == meta.id), "tag filter must match");
    let miss = kernel
        .list_files(owner, None, Some("nope"), None)
        .await
        .unwrap();
    assert!(
        !miss.iter().any(|f| f.id == meta.id),
        "non-matching tag excludes"
    );

    // Download returns the allowlisted MIME (png) and the exact bytes.
    let (ct, name, bytes) = kernel
        .download_file(owner, &meta.id)
        .await
        .expect("download");
    assert_eq!(ct, "image/png");
    assert_eq!(name, "pic.png");
    assert_eq!(bytes, png);

    // get then delete then 404.
    assert!(kernel.get_file(owner, &meta.id).await.is_ok());
    kernel.delete_file(owner, &meta.id).await.expect("delete");
    assert!(matches!(
        kernel.get_file(owner, &meta.id).await,
        Err(agentos_api::ApiError::NotFound(_))
    ));
    kernel.shutdown();
}

/// SVG is never served with its declared (script-capable) type on download.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn file_svg_download_is_neutralized() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let owner = "alice";
    let meta = kernel
        .upload_file(
            owner,
            "x.svg",
            "image/svg+xml",
            "global",
            &[],
            b"<svg/>".to_vec(),
        )
        .await
        .expect("upload svg");
    let (ct, _, _) = kernel
        .download_file(owner, &meta.id)
        .await
        .expect("download");
    assert_eq!(
        ct, "application/octet-stream",
        "svg must be neutralized to octet-stream"
    );
    kernel.shutdown();
}

/// A file uploaded by one owner is invisible to another (owner-principal scoping).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn file_owner_isolation() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let meta = kernel
        .upload_file(
            "alice",
            "secret.txt",
            "text/plain",
            "global",
            &[],
            b"top secret".to_vec(),
        )
        .await
        .expect("upload");

    // Bob cannot get or list alice's file.
    assert!(matches!(
        kernel.get_file("bob", &meta.id).await,
        Err(agentos_api::ApiError::NotFound(_))
    ));
    let bob_list = kernel.list_files("bob", None, None, None).await.unwrap();
    assert!(
        !bob_list.iter().any(|f| f.id == meta.id),
        "bob must not see alice's file"
    );
    kernel.shutdown();
}

// ── mark_notification_read ───────────────────────────────────────────────────

/// Write one unread notification straight into the inbox and return its id.
async fn seed_notification(kernel: &Kernel) -> agentos_types::NotificationID {
    use agentos_types::{
        NotificationID, NotificationPriority, NotificationSource, TraceID, UserMessage,
        UserMessageKind,
    };

    let msg = UserMessage {
        id: NotificationID::new(),
        from: NotificationSource::Kernel,
        task_id: None,
        trace_id: TraceID::new(),
        kind: UserMessageKind::Notification,
        priority: NotificationPriority::Info,
        subject: "test".to_string(),
        body: "test body".to_string(),
        interaction: None,
        delivery_status: Default::default(),
        response: None,
        created_at: chrono::Utc::now(),
        expires_at: None,
        read: false,
        thread_id: None,
        reply_to_external_id: None,
        attachment: None,
    };
    kernel
        .notification_router
        .inbox()
        .write(&msg)
        .await
        .expect("write notification");
    msg.id
}

/// Marking a notification read flips the row and drops the unread count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_mark_notification_read_drops_unread_count() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let id = seed_notification(&kernel).await;
    assert_eq!(kernel.get_unread_count().await.unwrap(), 1);

    let updated = kernel
        .mark_notification_read(id)
        .await
        .expect("mark_notification_read");
    assert!(updated, "expected the row to be updated");
    assert_eq!(
        kernel.get_unread_count().await.unwrap(),
        0,
        "unread count must drop after mark read"
    );

    // Idempotent: re-marking still matches the row, so `updated` stays true.
    assert!(
        kernel.mark_notification_read(id).await.unwrap(),
        "re-marking an existing notification is a no-op, not an error"
    );
    kernel.shutdown();
}

/// An unknown id is idempotent — Ok(false), not an error and not a 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_mark_notification_read_unknown_id_is_idempotent() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let updated = kernel
        .mark_notification_read(agentos_types::NotificationID::new())
        .await
        .expect("unknown id must not error");
    assert!(!updated, "unknown id reports updated = false");
    kernel.shutdown();
}

// ── Integrations: add-from-the-UI paths (plan: panel-integrations-wiring) ────

/// Clearing the inbox must spare a live blocking question: an `ask_user` task is
/// parked on its row, and deleting it makes the eventual response fail with
/// "not found or already has a response".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn clear_all_notifications_spares_live_questions() {
    use agentos_types::{
        NotificationID, NotificationPriority, NotificationSource, TraceID, UserMessage,
        UserMessageKind,
    };

    let (kernel, _tmp) = boot_test_kernel().await;
    let inbox = kernel.notification_router.inbox();

    let msg = |kind: UserMessageKind, interactive: bool| UserMessage {
        id: NotificationID::new(),
        from: NotificationSource::Kernel,
        task_id: None,
        trace_id: TraceID::new(),
        kind,
        priority: NotificationPriority::Info,
        subject: "s".into(),
        body: "b".into(),
        interaction: interactive.then(|| agentos_types::InteractionRequest {
            timeout_secs: 300,
            auto_action: "deny".into(),
            blocking: true,
            max_concurrent: 3,
        }),
        delivery_status: Default::default(),
        response: None,
        created_at: chrono::Utc::now(),
        // No deadline → still answerable, so the purge must keep it.
        expires_at: None,
        read: false,
        thread_id: None,
        reply_to_external_id: None,
        attachment: None,
    };

    inbox
        .write(&msg(UserMessageKind::Notification, false))
        .await
        .unwrap();
    inbox
        .write(&msg(UserMessageKind::Notification, false))
        .await
        .unwrap();
    let question = msg(
        UserMessageKind::Question {
            question: "Proceed?".into(),
            options: Some(vec!["yes".into(), "no".into()]),
            free_text_allowed: true,
        },
        true,
    );
    let question_id = question.id;
    inbox.write(&question).await.unwrap();

    assert_eq!(kernel.get_unread_count().await.unwrap(), 3);
    kernel.mark_all_notifications_read().await.unwrap();
    assert_eq!(
        kernel.get_unread_count().await.unwrap(),
        0,
        "mark-all-read must clear the badge"
    );

    let deleted = kernel.clear_all_notifications().await.unwrap();
    assert_eq!(deleted, 2, "the two plain notifications go");
    assert!(
        inbox.get(&question_id).await.unwrap().is_some(),
        "the unanswered question must survive — a task is waiting on it"
    );
}

/// The catalog is embedded in the binary, so it is populated on a fresh kernel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn mcp_catalog_lists_and_filters_seed_entries() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let all = kernel.list_mcp_catalog(None).await.unwrap();
    assert!(!all.is_empty(), "embedded catalog seeds must be listed");
    assert!(
        all.iter().all(|e| !e.installed),
        "nothing is attached on a fresh kernel"
    );

    let filtered = kernel.list_mcp_catalog(Some("filesystem")).await.unwrap();
    assert!(
        filtered.iter().any(|e| e.id == "filesystem"),
        "search must find the filesystem seed"
    );
    assert!(filtered.len() < all.len(), "search must actually narrow");

    let detail = kernel.get_mcp_catalog_entry("filesystem").await.unwrap();
    assert_eq!(
        detail.get("id").and_then(|v| v.as_str()),
        Some("filesystem")
    );
    assert!(matches!(
        kernel.get_mcp_catalog_entry("no-such-entry").await,
        Err(ApiError::NotFound(_))
    ));
    assert!(matches!(
        kernel
            .install_mcp_server("no-such-entry", Default::default())
            .await,
        Err(ApiError::NotFound(_))
    ));
}

/// The transport fields are mutually exclusive; the kernel only checks the auth
/// pair, so an ambiguous request has to be rejected at the API boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn attach_mcp_rejects_ambiguous_and_malformed_requests() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let req = |name: &str, command: Option<&str>, url: Option<&str>| AttachMcpRequest {
        name: name.to_string(),
        command: command.map(str::to_string),
        args: Vec::new(),
        url: url.map(str::to_string),
        auth_token: None,
        oauth_connector_id: None,
        timeout_secs: None,
        env: Default::default(),
    };

    assert!(
        matches!(
            kernel
                .attach_mcp_server(req("both", Some("npx"), Some("http://x")))
                .await,
            Err(ApiError::BadRequest(_))
        ),
        "command + url is ambiguous"
    );
    assert!(
        matches!(
            kernel.attach_mcp_server(req("neither", None, None)).await,
            Err(ApiError::BadRequest(_))
        ),
        "one transport is required"
    );
    assert!(
        matches!(
            kernel
                .attach_mcp_server(req("../escape", Some("npx"), None))
                .await,
            Err(ApiError::BadRequest(_))
        ),
        "the name is used as a persistence key — no traversal"
    );

    let mut auth_clash = req("clash", None, Some("https://example.com/mcp"));
    auth_clash.auth_token = Some(zeroize::Zeroizing::new("t".into()));
    auth_clash.oauth_connector_id = Some("c".into());
    assert!(matches!(
        kernel.attach_mcp_server(auth_clash).await,
        Err(ApiError::BadRequest(_))
    ));
}

/// `ChannelKind::from_str` is infallible (unknown → `Custom`), so an unsupported
/// kind must fail closed instead of registering a channel with no adapter that
/// still reports "connected".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn connect_channel_validates_kind_and_registers_ntfy() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let req = |kind: &str, name: &str, external: Option<&str>| ConnectChannelRequest {
        kind: kind.to_string(),
        display_name: name.to_string(),
        external_id: external.map(str::to_string),
        credential_key: None,
        credential: None,
        reply_topic: None,
        server_url: None,
        webhook_url: None,
        active_agent_name: None,
    };

    assert!(matches!(
        kernel.connect_channel(req("nope", "Bogus", None)).await,
        Err(ApiError::BadRequest(_))
    ));
    assert!(matches!(
        kernel.connect_channel(req("ntfy", "   ", Some("t"))).await,
        Err(ApiError::BadRequest(_))
    ));

    let ch = kernel
        .connect_channel(req("ntfy", "Ops alerts", Some("agentos-test-topic")))
        .await
        .expect("ntfy needs no credential");
    assert_eq!(ch.kind, "ntfy");
    assert_eq!(ch.display_name, "Ops alerts");
    assert!(kernel
        .list_channels()
        .await
        .unwrap()
        .iter()
        .any(|c| c.id == ch.id));

    // An unknown agent must be rejected, not silently stored.
    assert!(matches!(
        kernel
            .set_channel_agent(&ch.id, Some("no-such-agent".into()))
            .await,
        Err(ApiError::NotFound(_))
    ));
    assert!(matches!(
        kernel.set_channel_agent("not-a-uuid", None).await,
        Err(ApiError::BadRequest(_))
    ));
    assert!(matches!(
        kernel.test_channel("not-a-uuid").await,
        Err(ApiError::BadRequest(_))
    ));

    let pairings = kernel.list_pairings().await.unwrap();
    assert!(pairings.approved.is_empty() && pairings.pending.is_empty());

    kernel.disconnect_channel(&ch.id).await.unwrap();
    assert!(kernel.list_channels().await.unwrap().is_empty());
}

/// An edit keeps the channel's identity: same `ChannelInstanceID`, same row,
/// with only the named fields changed. Omitted stays, `""` clears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn update_channel_edits_in_place() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let ch = kernel
        .connect_channel(ConnectChannelRequest {
            kind: "ntfy".into(),
            display_name: "Ops alerts".into(),
            external_id: Some("agentos-test-topic".into()),
            credential_key: None,
            credential: None,
            reply_topic: Some("agentos-replies".into()),
            server_url: Some("https://ntfy.sh".into()),
            webhook_url: None,
            active_agent_name: None,
        })
        .await
        .expect("ntfy needs no credential");

    let edited = kernel
        .update_channel(
            &ch.id,
            UpdateChannelRequest {
                display_name: Some("Ops alerts (eu)".into()),
                external_id: Some("agentos-eu-topic".into()),
                // Omitted: server_url must survive untouched.
                reply_topic: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .expect("edit applies");

    assert_eq!(edited.id, ch.id, "the instance id is the identity");
    assert_eq!(edited.display_name, "Ops alerts (eu)");
    assert_eq!(edited.external_id, "agentos-eu-topic");
    assert_eq!(edited.reply_topic, None, "empty string clears the field");
    assert_eq!(edited.server_url.as_deref(), Some("https://ntfy.sh"));
    assert_eq!(edited.connected_at, ch.connected_at);
    // One row, not two.
    let rows = kernel.list_channels().await.unwrap();
    assert_eq!(rows.len(), 1);

    assert!(
        matches!(
            kernel
                .update_channel(
                    &ch.id,
                    UpdateChannelRequest {
                        display_name: Some("   ".into()),
                        ..Default::default()
                    },
                )
                .await,
            Err(ApiError::Conflict(_)) | Err(ApiError::BadRequest(_))
        ),
        "a blank name is refused"
    );
    assert!(matches!(
        kernel
            .update_channel(
                &ch.id,
                UpdateChannelRequest {
                    active_agent_name: Some("no-such-agent".into()),
                    ..Default::default()
                },
            )
            .await,
        Err(ApiError::NotFound(_))
    ));
    assert!(matches!(
        kernel
            .update_channel("not-a-uuid", UpdateChannelRequest::default())
            .await,
        Err(ApiError::BadRequest(_))
    ));

    kernel.disconnect_channel(&ch.id).await.unwrap();
}

/// The whole point of the edit path: a blank credential means *keep*. An edit
/// that does not mention the secret must leave the vault entry byte-identical,
/// and a `credential_key` outside the `channel.*` namespace must be refused —
/// `channels:w` is not `secrets:w`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn update_channel_keeps_the_stored_secret() {
    let (kernel, _tmp) = boot_test_kernel().await;
    let key = "channel.ntfy.protected";

    let ch = kernel
        .connect_channel(ConnectChannelRequest {
            kind: "ntfy".into(),
            display_name: "Protected".into(),
            external_id: Some("agentos-protected".into()),
            credential_key: Some(key.into()),
            credential: Some(zeroize::Zeroizing::new("original-token".into())),
            reply_topic: None,
            server_url: None,
            webhook_url: None,
            active_agent_name: None,
        })
        .await
        .expect("connects with an inline credential");

    kernel
        .update_channel(
            &ch.id,
            UpdateChannelRequest {
                display_name: Some("Protected (eu)".into()),
                ..Default::default()
            },
        )
        .await
        .expect("edit applies");
    assert_eq!(
        kernel.vault.get(key).await.unwrap().as_str(),
        "original-token",
        "an edit that never mentions the credential must not touch it"
    );

    // A caller-named key outside the channel namespace would let `channels:w`
    // overwrite any secret in the vault.
    assert!(matches!(
        kernel
            .update_channel(
                &ch.id,
                UpdateChannelRequest {
                    credential_key: Some("mcp.gmail.auth_token".into()),
                    credential: Some(zeroize::Zeroizing::new("attacker".into())),
                    ..Default::default()
                },
            )
            .await,
        Err(ApiError::BadRequest(_))
    ));
    assert!(
        kernel.vault.get("mcp.gmail.auth_token").await.is_err(),
        "the refused key must not have been written"
    );

    // A rotation through the channel's own key does land.
    kernel
        .update_channel(
            &ch.id,
            UpdateChannelRequest {
                credential: Some(zeroize::Zeroizing::new("rotated-token".into())),
                ..Default::default()
            },
        )
        .await
        .expect("rotation applies");
    assert_eq!(
        kernel.vault.get(key).await.unwrap().as_str(),
        "rotated-token"
    );

    kernel.disconnect_channel(&ch.id).await.unwrap();
}

/// Install writes under `plugins/user`, discovery picks it up, and removal takes
/// both the registry entry and the files. Bundled plugins must refuse removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn plugin_install_and_remove_roundtrip() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let manifest = r#"
id = "demo-plugin"
display_name = "Demo"
version = "0.1.0"
description = "A test plugin"
trust_tier = "core"
"#;

    let summary = kernel.install_plugin(manifest).await.unwrap();
    assert_eq!(summary.id, "demo-plugin");
    assert_eq!(summary.status, "discovered");
    assert!(summary.user_installed, "removable from the UI");

    let path = kernel.user_plugins_dir().join("demo-plugin/plugin.toml");
    assert!(path.exists(), "manifest persisted for the next boot");

    assert!(
        matches!(
            kernel.install_plugin(manifest).await,
            Err(ApiError::Conflict(_))
        ),
        "a second install of the same id must not overwrite the first"
    );
    assert!(matches!(
        kernel.install_plugin("id = \"x\"").await,
        Err(ApiError::BadRequest(_))
    ));
    assert!(
        matches!(
            kernel
                .install_plugin(
                    "id = \"../evil\"\ndisplay_name = \"E\"\nversion = \"1\"\ndescription = \"d\""
                )
                .await,
            Err(ApiError::BadRequest(_))
        ),
        "the id becomes a directory name — no traversal"
    );

    // A second plugin proves removal deletes only its own directory.
    kernel
        .install_plugin(
            "id = \"other-plugin\"\ndisplay_name = \"Other\"\nversion = \"0.1.0\"\ndescription = \"d\"\ntrust_tier = \"core\"",
        )
        .await
        .unwrap();

    kernel.remove_plugin("demo-plugin").await.unwrap();
    assert!(!path.exists(), "files deleted");
    assert!(
        kernel
            .user_plugins_dir()
            .join("other-plugin/plugin.toml")
            .exists(),
        "removing one plugin must not touch its neighbours"
    );
    assert!(kernel
        .list_plugins()
        .await
        .unwrap()
        .iter()
        .all(|p| p.id != "demo-plugin"));
    assert!(matches!(
        kernel.remove_plugin("demo-plugin").await,
        Err(ApiError::NotFound(_))
    ));
}

/// A connector manifest added over REST is registered immediately and persisted
/// where the boot loader will find it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn connector_add_credential_and_remove_roundtrip() {
    let (kernel, _tmp) = boot_test_kernel().await;

    let manifest = r#"
[connector]
id = "demo-api"
name = "Demo API"
version = "0.1.0"
description = "A test connector"
base_url = "https://api.example.com"

[connector.auth]
type = "oauth2"
scopes = ["read"]

[[tools]]
name = "list-things"
description = "List things"
method = "get"
path = "/things"
"#;

    let detail = kernel.add_connector(manifest).await.unwrap();
    assert_eq!(detail.id, "demo-api");
    assert_eq!(detail.tools, vec!["demo-api.list-things".to_string()]);
    assert!(!detail.connected, "no credential stored yet");
    assert!(kernel.connectors_dir().join("demo-api.toml").exists());

    let listed = kernel.list_connectors().await.unwrap();
    let row = listed.iter().find(|c| c.id == "demo-api").unwrap();
    assert!(row.registered);
    assert!(!row.connected);

    assert!(matches!(
        kernel.add_connector(manifest).await,
        Err(ApiError::Conflict(_))
    ));
    assert!(matches!(
        kernel.add_connector("not valid toml {{").await,
        Err(ApiError::BadRequest(_))
    ));

    kernel
        .store_connector_credential(
            "demo-api",
            StoreCredentialRequest {
                provider: None,
                access_token: zeroize::Zeroizing::new("tok".into()),
                refresh_token: None,
                token_endpoint: Some("https://provider.example.com/oauth/token".into()),
                client_id: None,
                client_secret: None,
                scopes: vec!["read".into()],
                expires_in_secs: None,
            },
        )
        .await
        .unwrap();
    let row = kernel
        .list_connectors()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.id == "demo-api")
        .unwrap();
    assert!(row.connected, "stored credential shows as connected");
    assert!(!row.oauth_available, "no provider block configured");

    // An empty token would store an unusable credential that still reads
    // "connected"; a non-HTTPS endpoint is refused by the vault's SSRF guard, so
    // both must fail as a 400 rather than a leaked internal error.
    let bad = |token: &str, endpoint: Option<&str>| StoreCredentialRequest {
        provider: None,
        access_token: zeroize::Zeroizing::new(token.into()),
        refresh_token: None,
        token_endpoint: endpoint.map(str::to_string),
        client_id: None,
        client_secret: None,
        scopes: vec![],
        expires_in_secs: None,
    };
    assert!(matches!(
        kernel
            .store_connector_credential("demo-api", bad("  ", Some("https://p.example/t")))
            .await,
        Err(ApiError::BadRequest(_))
    ));
    assert!(matches!(
        kernel
            .store_connector_credential("demo-api", bad("tok", Some("http://p.example/t")))
            .await,
        Err(ApiError::BadRequest(_))
    ));
    assert!(matches!(
        kernel
            .store_connector_credential("demo-api", bad("tok", None))
            .await,
        Err(ApiError::BadRequest(_))
    ));

    kernel.remove_connector("demo-api").await.unwrap();
    assert!(!kernel.connectors_dir().join("demo-api.toml").exists());
    assert!(kernel
        .list_connectors()
        .await
        .unwrap()
        .iter()
        .all(|c| c.id != "demo-api"));
    assert!(matches!(
        kernel.remove_connector("demo-api").await,
        Err(ApiError::NotFound(_))
    ));
}

/// No OAuth provider blocks are configured in a temp kernel, so a start attempt
/// must 404 rather than mint a pending flow that can never complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn connector_oauth_start_requires_a_configured_provider() {
    let (kernel, tmp) = boot_test_kernel().await;
    // Point the provider loader at this temp dir (no oauth_providers.toml there).
    std::env::set_var("AGENTOS_CONFIG", tmp.path().join("config.toml"));
    assert!(matches!(
        kernel
            .start_connector_oauth("github", "http://localhost:8080/cb")
            .await,
        Err(ApiError::NotFound(_))
    ));
}
