//! Integration tests for the KernelService trait implementation.
//!
//! Each test boots a real kernel into a temp directory, calls KernelService
//! methods directly on the `Kernel`, and asserts expected behaviour.

use agentos_api::types::{AuditFilter, TaskFilter};
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
            per_agent_rate_limit: 0,
            events: Default::default(),
            sandbox_policy: Default::default(),
            max_concurrent_sandbox_children: 4,
            context_compaction: Default::default(),
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
        chat: Default::default(),
        user_adaptation: Default::default(),
        env: Default::default(),
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
