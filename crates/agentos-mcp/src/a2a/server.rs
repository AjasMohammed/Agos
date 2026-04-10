/// A2A HTTP server — expose AgentOS as an A2A-compliant agent.
///
/// Endpoints:
///   GET  /.well-known/agent.json    — Agent Card (discoverable identity)
///   POST /a2a/tasks                  — Submit a task delegation
///   GET  /a2a/tasks/{id}             — Poll task status (auth required)
///   POST /a2a/tasks/{id}/cancel      — Cancel a running task
use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Maximum A2A request body size (256 KiB).
const MAX_BODY_BYTES: usize = 256 * 1024;

use super::agent_card::AgentCard;
use super::task::{A2ATask, A2ATaskStatus, CancelTaskRequest, SubmitTaskRequest};
use crate::server::McpAuthValidator;

/// Maximum number of terminal (completed/failed/cancelled) tasks to retain.
/// When exceeded, the oldest terminal tasks are pruned.
const MAX_TERMINAL_TASKS: usize = 500;

// ── Executor trait ─────────────────────────────────────────────────────────────

/// Abstraction over the kernel for executing A2A task delegations.
#[async_trait::async_trait]
pub trait A2ATaskExecutor: Send + Sync {
    /// Execute a capability by name with the given input.
    async fn execute_capability(
        &self,
        capability: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String>;

    /// Return the list of capabilities this executor supports.
    fn capabilities(&self) -> Vec<String>;
}

// ── App state ──────────────────────────────────────────────────────────────────

/// Per-task entry: task data + a cancellation token to stop background execution.
#[derive(Clone)]
struct TaskEntry {
    task: A2ATask,
    cancel: CancellationToken,
}

#[derive(Clone)]
pub struct A2AState {
    pub card: Arc<AgentCard>,
    pub executor: Arc<dyn A2ATaskExecutor>,
    pub auth: Arc<dyn McpAuthValidator>,
    /// In-memory task store (task_id → entry). Pruned when terminal tasks exceed MAX_TERMINAL_TASKS.
    tasks: Arc<RwLock<HashMap<String, TaskEntry>>>,
}

impl A2AState {
    /// Prune oldest terminal tasks if the store exceeds MAX_TERMINAL_TASKS terminal entries.
    async fn prune_terminal_tasks(&self) {
        let mut map = self.tasks.write().await;
        let terminal_count = map.values().filter(|e| e.task.is_terminal()).count();
        if terminal_count <= MAX_TERMINAL_TASKS {
            return;
        }
        // Collect terminal task IDs sorted by updated_at (oldest first)
        let mut terminal: Vec<(String, chrono::DateTime<chrono::Utc>)> = map
            .iter()
            .filter(|(_, e)| e.task.is_terminal())
            .map(|(id, e)| (id.clone(), e.task.updated_at))
            .collect();
        terminal.sort_by_key(|(_, t)| *t);
        let to_remove = terminal_count - MAX_TERMINAL_TASKS;
        for (id, _) in terminal.into_iter().take(to_remove) {
            map.remove(&id);
        }
    }
}

// ── Router ─────────────────────────────────────────────────────────────────────

pub fn build_a2a_router(
    card: AgentCard,
    executor: Arc<dyn A2ATaskExecutor>,
    auth: Arc<dyn McpAuthValidator>,
) -> Router {
    let state = A2AState {
        card: Arc::new(card),
        executor,
        auth,
        tasks: Arc::new(RwLock::new(HashMap::new())),
    };

    Router::new()
        .route("/.well-known/agent.json", get(handle_agent_card))
        .route("/a2a/tasks", post(handle_submit_task))
        .route("/a2a/tasks/{id}", get(handle_get_task))
        .route("/a2a/tasks/{id}/cancel", post(handle_cancel_task))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

// ── Handlers ───────────────────────────────────────────────────────────────────

async fn handle_agent_card(State(state): State<A2AState>) -> impl IntoResponse {
    Json((*state.card).clone())
}

async fn handle_submit_task(
    State(state): State<A2AState>,
    headers: HeaderMap,
    Json(req): Json<SubmitTaskRequest>,
) -> impl IntoResponse {
    // Auth check
    if let Err(msg) = validate_auth(&state.auth, &headers).await {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response();
    }

    // Validate capability is offered
    let caps = state.executor.capabilities();
    if !caps.contains(&req.capability) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Unknown capability '{}'. Available: {:?}", req.capability, caps)
            })),
        )
            .into_response();
    }

    let task_id = uuid::Uuid::new_v4().to_string();
    let task = A2ATask::new(
        task_id.clone(),
        req.sender.clone(),
        req.capability.clone(),
        req.input.clone(),
    );
    let cancel_token = CancellationToken::new();

    // Store task immediately as Submitted
    state.tasks.write().await.insert(
        task_id.clone(),
        TaskEntry {
            task: task.clone(),
            cancel: cancel_token.clone(),
        },
    );

    // Spawn execution in background; update task status when done or cancelled.
    let tasks = state.tasks.clone();
    let executor = state.executor.clone();
    let capability = req.capability.clone();
    let input = req.input.clone();
    let id = task_id.clone();

    tokio::spawn(async move {
        // Mark as Working
        {
            let mut map = tasks.write().await;
            if let Some(e) = map.get_mut(&id) {
                e.task.status = A2ATaskStatus::Working { message: None };
                e.task.updated_at = chrono::Utc::now();
            }
        }

        // Execute with cancellation support
        let result = tokio::select! {
            res = executor.execute_capability(&capability, input) => res,
            _ = cancel_token.cancelled() => {
                Err("Task was cancelled".to_string())
            }
        };

        // Mark as Completed, Failed, or Cancelled
        {
            let mut map = tasks.write().await;
            if let Some(e) = map.get_mut(&id) {
                // Only update if not already terminal (cancel handler may have beaten us)
                if !e.task.is_terminal() {
                    e.task.status = if cancel_token.is_cancelled() {
                        A2ATaskStatus::Cancelled
                    } else {
                        match result {
                            Ok(output) => A2ATaskStatus::Completed { output },
                            Err(error) => A2ATaskStatus::Failed { error },
                        }
                    };
                    e.task.updated_at = chrono::Utc::now();
                }
            }
        }

        // Prune old terminal tasks to prevent unbounded growth
        state.prune_terminal_tasks().await;
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"id": task_id, "status": "submitted"})),
    )
        .into_response()
}

async fn handle_get_task(
    State(state): State<A2AState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(msg) = validate_auth(&state.auth, &headers).await {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response();
    }
    let map = state.tasks.read().await;
    match map.get(&id) {
        Some(entry) => (StatusCode::OK, Json(entry.task.clone())).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Task '{}' not found", id)})),
        )
            .into_response(),
    }
}

async fn handle_cancel_task(
    State(state): State<A2AState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    _body: Option<Json<CancelTaskRequest>>,
) -> impl IntoResponse {
    if let Err(msg) = validate_auth(&state.auth, &headers).await {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response();
    }

    let mut map = state.tasks.write().await;
    match map.get_mut(&id) {
        Some(entry) if !entry.task.is_terminal() => {
            // Signal the background task to stop via the cancellation token.
            entry.cancel.cancel();
            // Also update status immediately so pollers see the cancellation right away.
            entry.task.status = A2ATaskStatus::Cancelled;
            entry.task.updated_at = chrono::Utc::now();
            (
                StatusCode::OK,
                Json(serde_json::json!({"id": id, "status": "cancelled"})),
            )
                .into_response()
        }
        Some(_) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Task is already in a terminal state"})),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Task '{}' not found", id)})),
        )
            .into_response(),
    }
}

// ── Auth helper ────────────────────────────────────────────────────────────────

async fn validate_auth(
    auth: &Arc<dyn McpAuthValidator>,
    headers: &HeaderMap,
) -> Result<(), String> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    auth.validate_token(token)
        .await
        .map_err(|e| format!("Unauthorized: {}", e))
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::agent_card::AuthRequirement;
    use crate::server::NoAuth;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::util::ServiceExt;

    struct MockExec;

    #[async_trait::async_trait]
    impl A2ATaskExecutor for MockExec {
        async fn execute_capability(
            &self,
            capability: &str,
            input: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            if capability == "echo" {
                Ok(input)
            } else {
                Err(format!("Unknown: {}", capability))
            }
        }

        fn capabilities(&self) -> Vec<String> {
            vec!["echo".to_string()]
        }
    }

    fn make_card() -> AgentCard {
        AgentCard {
            name: "test-agent".to_string(),
            description: "test".to_string(),
            url: "http://localhost:3001".to_string(),
            protocol_version: "1.0".to_string(),
            provider: "agentos".to_string(),
            version: "0.1.0".to_string(),
            capabilities: vec![],
            authentication: AuthRequirement::None,
        }
    }

    fn make_app() -> Router<()> {
        build_a2a_router(make_card(), Arc::new(MockExec), Arc::new(NoAuth))
    }

    #[tokio::test]
    async fn agent_card_served_at_well_known() {
        let app = make_app();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/.well-known/agent.json")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let card: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(card["name"], "test-agent");
        assert_eq!(card["provider"], "agentos");
    }

    #[tokio::test]
    async fn submit_task_returns_accepted() {
        let app = make_app();
        let body = serde_json::to_string(&serde_json::json!({
            "sender": "http://caller.example.com",
            "capability": "echo",
            "input": {"msg": "hello"}
        }))
        .unwrap();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/a2a/tasks")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["id"].as_str().is_some());
    }

    #[tokio::test]
    async fn submit_unknown_capability_returns_bad_request() {
        let app = make_app();
        let body = serde_json::to_string(&serde_json::json!({
            "sender": "http://caller.example.com",
            "capability": "nonexistent",
            "input": {}
        }))
        .unwrap();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/a2a/tasks")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_task_not_found_returns_404() {
        let app = make_app();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/a2a/tasks/nonexistent-id")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cancel_nonexistent_task_returns_404() {
        let app = make_app();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/a2a/tasks/bad-id/cancel")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_task_requires_auth() {
        struct RejectAll;
        #[async_trait::async_trait]
        impl McpAuthValidator for RejectAll {
            async fn validate_token(&self, _: &str) -> Result<(), String> {
                Err("forbidden".to_string())
            }
        }
        let app = build_a2a_router(make_card(), Arc::new(MockExec), Arc::new(RejectAll));
        let req = Request::builder()
            .method(Method::GET)
            .uri("/a2a/tasks/some-id")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
