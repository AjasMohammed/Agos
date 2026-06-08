//! Integration tests for the KernelService trait implementation.
//!
//! Each test boots a real kernel into a temp directory, calls KernelService
//! methods directly on the `Kernel`, and asserts expected behaviour.

use agentos_api::types::{AuditFilter, CreateChatSessionRequest, TaskFilter};
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

// ── HTTP-level integration: drive the real `build_router` end-to-end ────────

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
