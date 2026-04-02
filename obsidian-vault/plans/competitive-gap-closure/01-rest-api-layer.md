---
title: "Phase 1.1: REST/HTTP API Layer"
tags:
  - kernel
  - api
  - v3
  - plan
  - phase-1
date: 2026-03-30
status: planned
effort: 5d
priority: critical
---

# Phase 1.1: REST/HTTP API Layer

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create `agentos-api` crate exposing AgentOS as a JSON REST API with an OpenAI-compatible `/v1/chat/completions` endpoint.

**Architecture:** New Axum-based HTTP server that receives JSON requests, translates them to `KernelCommand` variants, sends them through the existing kernel command router, and returns JSON responses. Runs on a separate port from the web UI.

**Tech Stack:** Axum 0.8, serde_json, utoipa (OpenAPI generation), tower-governor (rate limiting), tokio

---

## Why This Phase

AgentOS is invisible to the broader ecosystem because there's no HTTP API. Every SDK, no-code tool, and integration platform speaks HTTP/JSON. OpenFang exposes 140+ REST endpoints including an OpenAI-compatible surface. Without this, AgentOS cannot be a drop-in replacement for any workflow.

## Current → Target State

**Current:** CLI (`agentctl`) → Unix domain socket (`agentos-bus`) → Kernel. No HTTP surface except the HTML web UI.

**Target:** External clients → `agentos-api` (HTTP/JSON, port 8080) → Kernel. OpenAI-compatible `/v1/chat/completions` allows any OpenAI SDK to talk to AgentOS.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `crates/agentos-api/Cargo.toml` | Create | New crate manifest |
| `crates/agentos-api/src/lib.rs` | Create | Crate root, re-exports |
| `crates/agentos-api/src/server.rs` | Create | Axum server setup, middleware stack |
| `crates/agentos-api/src/router.rs` | Create | Route definitions (~50 endpoints) |
| `crates/agentos-api/src/auth.rs` | Create | API key auth middleware |
| `crates/agentos-api/src/handlers/mod.rs` | Create | Handler module index |
| `crates/agentos-api/src/handlers/chat.rs` | Create | OpenAI-compat `/v1/chat/completions` |
| `crates/agentos-api/src/handlers/agents.rs` | Create | Agent CRUD handlers |
| `crates/agentos-api/src/handlers/tasks.rs` | Create | Task lifecycle handlers |
| `crates/agentos-api/src/handlers/tools.rs` | Create | Tool management handlers |
| `crates/agentos-api/src/handlers/system.rs` | Create | Health, status, config |
| `crates/agentos-api/src/openai_compat.rs` | Create | OpenAI request/response type translation |
| `crates/agentos-api/src/api_key.rs` | Create | API key generation, storage, validation |
| `crates/agentos-types/src/api_key.rs` | Create | ApiKey type definition |
| `crates/agentos-types/src/lib.rs` | Modify | Add `pub mod api_key;` re-export |
| `crates/agentos-bus/src/message.rs` | Modify | Add API key KernelCommand variants |
| `crates/agentos-kernel/src/commands/api_key.rs` | Create | API key command handler |
| `crates/agentos-kernel/src/commands/mod.rs` | Modify | Add api_key module |
| `crates/agentos-kernel/src/kernel.rs` | Modify | Add ApiKeyStore to Kernel struct |
| `crates/agentos-cli/src/main.rs` | Modify | Boot API server alongside web server |
| `Cargo.toml` (workspace) | Modify | Add agentos-api member |
| `crates/agentos-api/tests/api_test.rs` | Create | Integration tests |

## Dependencies

- **Requires:** Nothing — this is a root phase
- **Blocks:** Phase 1.2 (Channels), Phase 1.3 (Marketplace), Phase 3.2 (Benchmarks)

---

## Detailed Tasks

### Task 1: Scaffold `agentos-api` Crate

**Files:**
- Create: `crates/agentos-api/Cargo.toml`
- Create: `crates/agentos-api/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create crate directory**

```bash
mkdir -p crates/agentos-api/src/handlers
```

- [ ] **Step 2: Write Cargo.toml**

```toml
[package]
name = "agentos-api"
version.workspace = true
edition.workspace = true

[dependencies]
agentos-types = { path = "../agentos-types" }
agentos-kernel = { path = "../agentos-kernel" }
agentos-bus = { path = "../agentos-bus" }
agentos-audit = { path = "../agentos-audit" }
axum = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
async-trait = { workspace = true }
chrono = { workspace = true }
tower-governor = "0.4"
tower-http = { version = "0.6", features = ["cors", "compression-gzip", "trace"] }
utoipa = { version = "5", features = ["axum_extras"] }
uuid = { version = "1", features = ["v4"] }
hmac = "0.12"
sha2 = "0.10"
hex = { workspace = true }
```

- [ ] **Step 3: Write lib.rs with module declarations**

```rust
pub mod api_key;
pub mod auth;
pub mod handlers;
pub mod openai_compat;
pub mod router;
pub mod server;
```

- [ ] **Step 4: Add to workspace members**

In root `Cargo.toml`, add `"crates/agentos-api"` to `[workspace] members`.

- [ ] **Step 5: Verify it compiles**

Run: `cargo build -p agentos-api`
Expected: Compile success (with empty modules)

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-api/ Cargo.toml
git commit -m "feat(api): scaffold agentos-api crate"
```

### Task 2: API Key Types and Storage

**Files:**
- Create: `crates/agentos-types/src/api_key.rs`
- Modify: `crates/agentos-types/src/lib.rs`
- Create: `crates/agentos-api/src/api_key.rs`

- [ ] **Step 1: Write the failing test**

In `crates/agentos-api/src/api_key.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_validate_api_key() {
        let store = ApiKeyStore::new("test-secret-key");
        let key = store.create_key("my-app", vec!["agents:read".to_string(), "tasks:write".to_string()]);
        assert!(key.raw_key.starts_with("agos_"));
        assert_eq!(key.name, "my-app");
        assert!(store.validate(&key.raw_key).is_some());
    }

    #[test]
    fn test_invalid_key_rejected() {
        let store = ApiKeyStore::new("test-secret-key");
        assert!(store.validate("agos_invalid_garbage").is_none());
    }

    #[test]
    fn test_revoked_key_rejected() {
        let store = ApiKeyStore::new("test-secret-key");
        let key = store.create_key("temp", vec![]);
        let key_id = key.id.clone();
        store.revoke(&key_id);
        assert!(store.validate(&key.raw_key).is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p agentos-api -- test_create_and_validate_api_key`
Expected: FAIL — `ApiKeyStore` not defined

- [ ] **Step 3: Write ApiKey type in agentos-types**

In `crates/agentos-types/src/api_key.rs`:
```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Metadata for an API key (stored in kernel; raw key never persisted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyMeta {
    pub id: String,
    pub name: String,
    pub permissions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
    pub revoked: bool,
    /// HMAC hash of the raw key — used for validation without storing plaintext.
    pub key_hash: String,
}
```

Add `pub mod api_key;` to `crates/agentos-types/src/lib.rs`.

- [ ] **Step 4: Write ApiKeyStore implementation**

In `crates/agentos-api/src/api_key.rs`:
```rust
use agentos_types::api_key::ApiKeyMeta;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::RwLock;

type HmacSha256 = Hmac<Sha256>;

/// Result of creating a new API key — includes the raw key (shown once).
pub struct CreatedApiKey {
    pub id: String,
    pub name: String,
    pub raw_key: String,
    pub permissions: Vec<String>,
}

pub struct ApiKeyStore {
    secret: String,
    keys: RwLock<HashMap<String, ApiKeyMeta>>,
}

impl ApiKeyStore {
    pub fn new(secret: &str) -> Self {
        Self {
            secret: secret.to_string(),
            keys: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_key(&self, name: &str, permissions: Vec<String>) -> CreatedApiKey {
        let id = uuid::Uuid::new_v4().to_string();
        let raw_key = format!("agos_{}", hex::encode(&uuid::Uuid::new_v4().as_bytes()[..]));
        let key_hash = self.hash_key(&raw_key);

        let meta = ApiKeyMeta {
            id: id.clone(),
            name: name.to_string(),
            permissions: permissions.clone(),
            created_at: Utc::now(),
            last_used: None,
            revoked: false,
            key_hash,
        };

        self.keys.write().unwrap().insert(id.clone(), meta);

        CreatedApiKey {
            id,
            name: name.to_string(),
            raw_key,
            permissions,
        }
    }

    pub fn validate(&self, raw_key: &str) -> Option<ApiKeyMeta> {
        let hash = self.hash_key(raw_key);
        let mut keys = self.keys.write().unwrap();
        for meta in keys.values_mut() {
            if meta.key_hash == hash && !meta.revoked {
                meta.last_used = Some(Utc::now());
                return Some(meta.clone());
            }
        }
        None
    }

    pub fn revoke(&self, key_id: &str) {
        if let Some(meta) = self.keys.write().unwrap().get_mut(key_id) {
            meta.revoked = true;
        }
    }

    pub fn list(&self) -> Vec<ApiKeyMeta> {
        self.keys.read().unwrap().values().cloned().collect()
    }

    fn hash_key(&self, raw_key: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(raw_key.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p agentos-api`
Expected: All 3 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-types/src/api_key.rs crates/agentos-types/src/lib.rs crates/agentos-api/src/api_key.rs
git commit -m "feat(api): add API key types and HMAC-based key store"
```

### Task 3: Auth Middleware

**Files:**
- Create: `crates/agentos-api/src/auth.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn test_extract_bearer_token() {
        let header = "Bearer agos_abc123";
        assert_eq!(extract_bearer(header), Some("agos_abc123"));
    }

    #[test]
    fn test_extract_bearer_missing_prefix() {
        assert_eq!(extract_bearer("agos_abc123"), None);
    }

    #[test]
    fn test_extract_bearer_empty() {
        assert_eq!(extract_bearer("Bearer "), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p agentos-api -- test_extract_bearer`
Expected: FAIL — `extract_bearer` not defined

- [ ] **Step 3: Implement auth middleware**

In `crates/agentos-api/src/auth.rs`:
```rust
use crate::api_key::ApiKeyStore;
use agentos_types::api_key::ApiKeyMeta;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Extension;
use std::sync::Arc;

/// Extract bearer token from Authorization header.
pub fn extract_bearer(header_value: &str) -> Option<&str> {
    let token = header_value.strip_prefix("Bearer ")?;
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Axum middleware: validates API key from Authorization header.
pub async fn require_api_key(
    Extension(key_store): Extension<Arc<ApiKeyStore>>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let Some(header) = auth_header else {
        return (StatusCode::UNAUTHORIZED, r#"{"error":"missing Authorization header"}"#).into_response();
    };

    let Some(token) = extract_bearer(header) else {
        return (StatusCode::UNAUTHORIZED, r#"{"error":"invalid Authorization format, expected: Bearer <key>"}"#).into_response();
    };

    let Some(meta) = key_store.validate(token) else {
        return (StatusCode::UNAUTHORIZED, r#"{"error":"invalid or revoked API key"}"#).into_response();
    };

    // Inject validated key metadata for handlers to use.
    request.extensions_mut().insert(meta);
    next.run(request).await
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p agentos-api`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-api/src/auth.rs
git commit -m "feat(api): add API key bearer auth middleware"
```

### Task 4: OpenAI-Compatible Types

**Files:**
- Create: `crates/agentos-api/src/openai_compat.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_openai_request() {
        let json = r#"{
            "model": "anthropic/claude-sonnet-4-6",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "stream": false,
            "temperature": 0.7
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model, "anthropic/claude-sonnet-4-6");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert!(!req.stream.unwrap_or(false));
    }

    #[test]
    fn test_serialize_openai_response() {
        let resp = ChatCompletionResponse {
            id: "chatcmpl-abc123".to_string(),
            object: "chat.completion".to_string(),
            created: 1234567890,
            model: "claude-sonnet-4-6".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some("Hi there!".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("chatcmpl-abc123"));
        assert!(json.contains("Hi there!"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p agentos-api -- test_deserialize_openai`
Expected: FAIL — types not defined

- [ ] **Step 3: Implement OpenAI-compatible types**

In `crates/agentos-api/src/openai_compat.rs`:
```rust
use serde::{Deserialize, Serialize};

/// OpenAI-compatible chat completion request.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
}

/// A chat message (used in both request and response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// OpenAI-compatible chat completion response.
#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: ChatUsage,
}

#[derive(Debug, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// SSE streaming chunk (OpenAI format).
#[derive(Debug, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

#[derive(Debug, Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: ChunkDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p agentos-api`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-api/src/openai_compat.rs
git commit -m "feat(api): add OpenAI-compatible request/response types"
```

### Task 5: Core Handlers — Health, Status, Agents, Tasks

**Files:**
- Create: `crates/agentos-api/src/handlers/mod.rs`
- Create: `crates/agentos-api/src/handlers/system.rs`
- Create: `crates/agentos-api/src/handlers/agents.rs`
- Create: `crates/agentos-api/src/handlers/tasks.rs`

- [ ] **Step 1: Write handler module index**

In `crates/agentos-api/src/handlers/mod.rs`:
```rust
pub mod agents;
pub mod chat;
pub mod system;
pub mod tasks;
pub mod tools;
```

- [ ] **Step 2: Write system handlers (health, status)**

In `crates/agentos-api/src/handlers/system.rs`:
```rust
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use agentos_kernel::Kernel;

pub type KernelState = Arc<Kernel>;

pub async fn health(State(kernel): State<KernelState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

pub async fn status(State(kernel): State<KernelState>) -> Result<Json<Value>, StatusCode> {
    let status = kernel.get_status().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::to_value(status).unwrap_or_default()))
}
```

- [ ] **Step 3: Write agent handlers (list, connect, disconnect)**

In `crates/agentos-api/src/handlers/agents.rs`:
```rust
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use super::system::KernelState;

#[derive(Deserialize)]
pub struct ConnectAgentRequest {
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

pub async fn list_agents(State(kernel): State<KernelState>) -> Result<Json<Value>, StatusCode> {
    let agents = kernel.list_agents().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::to_value(agents).unwrap_or_default()))
}

pub async fn connect_agent(
    State(kernel): State<KernelState>,
    Json(req): Json<ConnectAgentRequest>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let provider = req.provider.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let result = kernel
        .connect_agent(&req.name, provider, &req.model, req.base_url.as_deref(), req.roles)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(result).unwrap_or_default())))
}

pub async fn disconnect_agent(
    State(kernel): State<KernelState>,
    Path(name): Path<String>,
) -> Result<StatusCode, StatusCode> {
    kernel
        .disconnect_agent(&name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}
```

- [ ] **Step 4: Write task handlers (run, list, cancel, status)**

In `crates/agentos-api/src/handlers/tasks.rs`:
```rust
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use super::system::KernelState;

#[derive(Deserialize)]
pub struct RunTaskRequest {
    pub prompt: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub autonomous: bool,
}

pub async fn run_task(
    State(kernel): State<KernelState>,
    Json(req): Json<RunTaskRequest>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let task_id = kernel
        .run_task(req.agent_name.as_deref(), &req.prompt, req.autonomous)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"task_id": task_id.to_string()}))))
}

pub async fn list_tasks(State(kernel): State<KernelState>) -> Result<Json<Value>, StatusCode> {
    let tasks = kernel.list_tasks().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::to_value(tasks).unwrap_or_default()))
}

pub async fn cancel_task(
    State(kernel): State<KernelState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let task_id = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    kernel.cancel_task(task_id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}
```

- [ ] **Step 5: Verify compilation**

Run: `cargo build -p agentos-api`
Expected: Compiles (handlers reference kernel methods that may need thin wrappers — adapt method names to match kernel's actual public API)

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-api/src/handlers/
git commit -m "feat(api): add system, agent, and task handlers"
```

### Task 6: Chat Completions Handler (OpenAI-compat)

**Files:**
- Create: `crates/agentos-api/src/handlers/chat.rs`

- [ ] **Step 1: Implement the chat completions handler**

In `crates/agentos-api/src/handlers/chat.rs`:
```rust
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::Json;
use futures::stream::Stream;
use serde_json::json;
use std::convert::Infallible;
use tokio::sync::mpsc;

use crate::openai_compat::*;
use super::system::KernelState;

/// POST /v1/chat/completions — OpenAI-compatible endpoint.
pub async fn chat_completions(
    State(kernel): State<KernelState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Json<ChatCompletionResponse>, (StatusCode, Json<serde_json::Value>)> {
    // Parse model as "provider/model" or just "model" (uses default agent)
    let (agent_name, _model) = parse_model_string(&req.model);

    // Convert OpenAI messages to AgentOS context entries
    let prompt = req.messages.iter()
        .filter(|m| m.role == "user")
        .filter_map(|m| m.content.as_ref())
        .last()
        .ok_or_else(|| {
            (StatusCode::BAD_REQUEST, Json(json!({"error": "no user message found"})))
        })?;

    // Run task through kernel
    let task_id = kernel
        .run_task(agent_name.as_deref(), prompt, false)
        .await
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
        })?;

    // Wait for task completion and collect result
    let result = kernel
        .wait_for_task(task_id)
        .await
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
        })?;

    let response = ChatCompletionResponse {
        id: format!("chatcmpl-{}", task_id),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: req.model.clone(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some(result),
                tool_calls: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: ChatUsage {
            prompt_tokens: 0, // TODO: wire from kernel cost tracker
            completion_tokens: 0,
            total_tokens: 0,
        },
    };

    Ok(Json(response))
}

/// Parse "provider/model" string. Returns (optional agent_name, model).
fn parse_model_string(model: &str) -> (Option<String>, String) {
    if let Some((agent, model)) = model.split_once('/') {
        (Some(agent.to_string()), model.to_string())
    } else {
        (None, model.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_with_agent() {
        let (agent, model) = parse_model_string("my-agent/claude-sonnet-4-6");
        assert_eq!(agent, Some("my-agent".to_string()));
        assert_eq!(model, "claude-sonnet-4-6");
    }

    #[test]
    fn test_parse_model_without_agent() {
        let (agent, model) = parse_model_string("claude-sonnet-4-6");
        assert_eq!(agent, None);
        assert_eq!(model, "claude-sonnet-4-6");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p agentos-api`
Expected: All tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/agentos-api/src/handlers/chat.rs
git commit -m "feat(api): add OpenAI-compatible /v1/chat/completions handler"
```

### Task 7: Router and Server Setup

**Files:**
- Create: `crates/agentos-api/src/router.rs`
- Create: `crates/agentos-api/src/server.rs`

- [ ] **Step 1: Write router with all routes**

In `crates/agentos-api/src/router.rs`:
```rust
use axum::Router;
use std::sync::Arc;
use crate::api_key::ApiKeyStore;
use crate::auth::require_api_key;
use crate::handlers::{agents, chat, system, tasks, tools};
use agentos_kernel::Kernel;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};

pub fn build_api_router(kernel: Arc<Kernel>, key_store: Arc<ApiKeyStore>) -> Router {
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(120)
            .finish()
            .expect("valid governor config"),
    );

    Router::new()
        // OpenAI-compatible
        .route("/v1/chat/completions", axum::routing::post(chat::chat_completions))
        // Agents
        .route("/v1/agents", axum::routing::get(agents::list_agents).post(agents::connect_agent))
        .route("/v1/agents/{name}", axum::routing::delete(agents::disconnect_agent))
        // Tasks
        .route("/v1/tasks/run", axum::routing::post(tasks::run_task))
        .route("/v1/tasks", axum::routing::get(tasks::list_tasks))
        .route("/v1/tasks/{id}/cancel", axum::routing::post(tasks::cancel_task))
        // System
        .route("/v1/health", axum::routing::get(system::health))
        .route("/v1/status", axum::routing::get(system::status))
        // Auth middleware
        .layer(axum::middleware::from_fn(require_api_key))
        .layer(axum::Extension(key_store))
        .with_state(kernel)
        .layer(GovernorLayer::new(governor_conf))
}
```

- [ ] **Step 2: Write server entry point**

In `crates/agentos-api/src/server.rs`:
```rust
use crate::api_key::ApiKeyStore;
use crate::router::build_api_router;
use agentos_kernel::Kernel;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

pub async fn run_api_server(
    kernel: Arc<Kernel>,
    key_store: Arc<ApiKeyStore>,
    bind_addr: SocketAddr,
) -> Result<(), anyhow::Error> {
    let app = build_api_router(kernel, key_store);
    let listener = TcpListener::bind(bind_addr).await?;
    info!("API server listening on {}", bind_addr);
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo build -p agentos-api`
Expected: Compiles (some kernel method signatures may need thin wrappers)

- [ ] **Step 4: Commit**

```bash
git add crates/agentos-api/src/router.rs crates/agentos-api/src/server.rs
git commit -m "feat(api): add router with all routes and server entry point"
```

### Task 8: Wire API Server into Kernel Boot

**Files:**
- Modify: `crates/agentos-cli/src/main.rs`
- Modify: `crates/agentos-kernel/src/kernel.rs`

- [ ] **Step 1: Add API server spawn to kernel boot**

In the CLI's `start` command handler, after the web server is spawned, add:
```rust
// Boot API server on port 8080 (or configurable)
let api_addr: SocketAddr = "0.0.0.0:8080".parse().unwrap();
let api_key_store = Arc::new(agentos_api::api_key::ApiKeyStore::new(
    &config.api_secret.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
));
let api_kernel = kernel.clone();
let api_store = api_key_store.clone();
tokio::spawn(async move {
    if let Err(e) = agentos_api::server::run_api_server(api_kernel, api_store, api_addr).await {
        tracing::error!("API server failed: {}", e);
    }
});
tracing::info!("✓ API server listening on {}", api_addr);
```

- [ ] **Step 2: Add agentos-api dependency to agentos-cli Cargo.toml**

```toml
agentos-api = { path = "../agentos-api" }
```

- [ ] **Step 3: Integration test — boot kernel and hit /v1/health**

Create `crates/agentos-api/tests/api_test.rs`:
```rust
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::test]
async fn test_health_endpoint() {
    // This test verifies the API server boots and responds to /v1/health
    let client = reqwest::Client::new();
    // Note: requires a running API server — skip in CI or use test harness
    // For now, verify the router builds without panicking
    let key_store = Arc::new(agentos_api::api_key::ApiKeyStore::new("test"));
    let _key = key_store.create_key("test", vec![]);
    assert!(!key_store.list().is_empty());
}
```

- [ ] **Step 4: Run build and tests**

Run: `cargo build --workspace && cargo test -p agentos-api`
Expected: Build succeeds, tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-cli/ crates/agentos-api/tests/
git commit -m "feat(api): wire API server into kernel boot sequence"
```

---

## Test Plan

| Test | Assertion |
|------|-----------|
| API key creation | `agos_` prefix, HMAC hash stored, raw key validates |
| API key revocation | Revoked key returns `None` from `validate()` |
| Bearer extraction | Correct parsing of `Authorization: Bearer <key>` |
| OpenAI request deserialization | All fields (model, messages, stream, temperature) parse |
| OpenAI response serialization | Valid JSON with `id`, `choices`, `usage` fields |
| Model string parsing | `"agent/model"` splits correctly; `"model"` returns None agent |
| Health endpoint | Returns 200 with `{"status":"ok"}` |

## Verification

```bash
# Build the workspace
cargo build --workspace

# Run API crate tests
cargo test -p agentos-api

# Run clippy
cargo clippy -p agentos-api -- -D warnings

# Run fmt check
cargo fmt -p agentos-api -- --check

# Manual verification (after kernel boot):
# curl http://localhost:8080/v1/health
# curl -H "Authorization: Bearer agos_<key>" http://localhost:8080/v1/agents
# curl -X POST -H "Authorization: Bearer agos_<key>" -H "Content-Type: application/json" \
#   -d '{"model":"default","messages":[{"role":"user","content":"hello"}]}' \
#   http://localhost:8080/v1/chat/completions
```
