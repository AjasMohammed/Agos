use agentos_tools::http_client::HttpClientTool;
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{
    AgentID, AgentOSError, PermissionSet, SecretOwner, SecretScope, TaskID, TraceID,
};
use agentos_vault::{ProxyVault, SecretsVault};
use serial_test::serial;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Helper to construct context with optional vault.
// Grants network.outbound:Execute so the tool's internal permission check
// passes; individual tests that need SSRF to fire still work because the URL
// is blocked before any TCP connection is made.
fn make_context(data_dir: &Path, vault: Option<Arc<ProxyVault>>) -> ToolExecutionContext {
    let mut permissions = PermissionSet::new();
    permissions.grant("network.outbound".to_string(), false, false, true, None);
    ToolExecutionContext {
        data_dir: data_dir.to_path_buf(),
        task_id: TaskID::new(),
        agent_id: AgentID::new(),
        trace_id: TraceID::new(),
        permissions,
        vault,
        hal: None,
        file_lock_registry: None,
        agent_registry: None,
        task_registry: None,
        workspace_paths: vec![],
        cancellation_token: tokio_util::sync::CancellationToken::new(),
    }
}

// Helper to init a temporary vault with a secret, returning a ProxyVault wrapper
async fn setup_temp_vault(dir: &TempDir, secret_name: &str, secret_value: &str) -> Arc<ProxyVault> {
    let db_path = dir.path().join("vault.db");
    let audit_db = dir.path().join("audit.db");
    let audit = Arc::new(agentos_audit::AuditLog::open(&audit_db).unwrap());

    let vault = SecretsVault::initialize(
        &db_path,
        &agentos_vault::ZeroizingString::new("pass".to_string()),
        audit,
    )
    .unwrap();
    vault
        .set(
            secret_name,
            secret_value,
            SecretOwner::Kernel,
            SecretScope::Global,
        )
        .await
        .unwrap();
    Arc::new(ProxyVault::new(Arc::new(vault)))
}

#[tokio::test]
#[serial]
async fn test_get_request_returns_json() {
    std::env::set_var("AGENTOS_TEST_ALLOW_LOCAL", "1");
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/data"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"success": true, "msg": "Hello"})),
        )
        .mount(&mock_server)
        .await;

    let dir = TempDir::new().unwrap();
    let ctx = make_context(dir.path(), None);
    let tool = HttpClientTool::new().unwrap();

    let payload = serde_json::json!({
        "url": format!("{}/api/data", mock_server.uri()),
        "method": "GET"
    });

    let result = tool.execute(payload, ctx).await.unwrap();

    assert_eq!(result["status"], 200);
    assert_eq!(result["body"]["success"], true);
    assert_eq!(result["body"]["msg"], "Hello");
}

#[tokio::test]
#[serial]
async fn test_ssrf_localhost_blocked() {
    // Explicitly disable the SSRF bypass for this specific test
    std::env::remove_var("AGENTOS_TEST_ALLOW_LOCAL");

    let dir = TempDir::new().unwrap();
    let ctx = make_context(dir.path(), None);
    let tool = HttpClientTool::new().unwrap();

    // Trying to fetch from localhost
    let payload = serde_json::json!({
        "url": "http://127.0.0.1:8080/admin",
        "method": "GET"
    });

    let err = tool.execute(payload, ctx).await.unwrap_err();
    match err {
        AgentOSError::PermissionDenied { operation, .. } => {
            assert!(operation.contains("blocked access to local/private IP"));
        }
        _ => panic!("Expected SSRF PermissionDenied error"),
    }
}

#[tokio::test]
#[serial]
async fn test_secret_header_injected_not_returned() {
    std::env::set_var("AGENTOS_TEST_ALLOW_LOCAL", "1");
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/secure"))
        .and(header("Authorization", "Bearer TOP_SECRET_123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})))
        .mount(&mock_server)
        .await;

    let dir = TempDir::new().unwrap();
    let vault = setup_temp_vault(&dir, "MY_ACTUAL_TOKEN", "TOP_SECRET_123").await;
    let ctx = make_context(dir.path(), Some(vault));
    let tool = HttpClientTool::new().unwrap();

    let payload = serde_json::json!({
        "url": format!("{}/api/secure", mock_server.uri()),
        "method": "POST",
        "secret_headers": {
            "Authorization": "Bearer $MY_ACTUAL_TOKEN"
        }
    });

    let result = tool.execute(payload, ctx).await.unwrap();

    assert_eq!(result["status"], 200);

    // Crucially: The secret MUST NOT be present in the returned stdout
    let result_str = serde_json::to_string(&result).unwrap();
    assert!(
        !result_str.contains("TOP_SECRET_123"),
        "Secret leaked into tool output!"
    );
}

#[tokio::test]
#[serial]
async fn test_timeout_respected() {
    std::env::set_var("AGENTOS_TEST_ALLOW_LOCAL", "1");
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1000)))
        .mount(&mock_server)
        .await;

    let dir = TempDir::new().unwrap();
    let ctx = make_context(dir.path(), None);
    let tool = HttpClientTool::new().unwrap();

    // Set timeout to 100ms
    let payload = serde_json::json!({
        "url": format!("{}/slow", mock_server.uri()),
        "method": "GET",
        "timeout_ms": 100
    });

    let err = tool.execute(payload, ctx).await.unwrap_err();
    match err {
        AgentOSError::ToolExecutionFailed { reason, .. } => {
            assert!(
                reason.contains("timed out") || reason.contains("timeout"),
                "Reason was: {}",
                reason
            );
        }
        _ => panic!("Expected ToolExecutionFailed error due to timeout"),
    }
}

#[tokio::test]
#[serial]
async fn test_large_response_truncated() {
    std::env::set_var("AGENTOS_TEST_ALLOW_LOCAL", "1");
    let mock_server = MockServer::start().await;

    // Generate an 11 MB string
    let huge_body = vec![b'A'; 11 * 1024 * 1024];

    Mock::given(method("GET"))
        .and(path("/huge"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(huge_body)
                .insert_header("Content-Type", "text/plain"),
        )
        .mount(&mock_server)
        .await;

    let dir = TempDir::new().unwrap();
    let ctx = make_context(dir.path(), None);
    let tool = HttpClientTool::new().unwrap();

    let payload = serde_json::json!({
        "url": format!("{}/huge", mock_server.uri()),
        "method": "GET"
    });

    let result = tool.execute(payload, ctx).await.unwrap();

    assert_eq!(result["status"], 200);
    assert_eq!(result["truncated"], true);

    let body_str = result["body"].as_str().unwrap();
    assert!(body_str.ends_with("[TRUNCATED to 10MB]"));
    assert!(body_str.len() <= 10 * 1024 * 1024 + 100); // 10MB + some buffer for the warning text
}
