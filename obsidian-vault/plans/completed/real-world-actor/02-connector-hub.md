---
title: "Phase 2: API Connector Hub"
tags:
  - plan
  - real-world
  - connectors
  - tools
  - phase-2
date: 2026-04-08
status: complete
effort: 3d
priority: high
---

# Phase 2: API Connector Hub

> A new `agentos-connectors` crate that lets agents call external APIs (GitHub, Slack, Stripe) through namespaced tools, with credential injection handled transparently by the kernel.

---

## Why This Phase

Today, an agent calling a GitHub API must:
1. Retrieve a token from the vault (or worse, have it in context)
2. Construct the HTTP request manually using `http-client`
3. Handle auth headers, pagination, error codes, and rate limits itself

This burns LLM tokens on boilerplate and risks credential leakage into context/memory. The Connector Hub abstracts this: the agent calls `github.create_issue(repo, title, body)` and the OS handles auth, HTTP construction, and error translation.

---

## Current State

- `http-client` tool exists — raw HTTP with no auth injection
- `agentos-vault` stores secrets and (after Phase 1) OAuth credentials
- `agentos-tools` has the `AgentTool` trait and `ToolManifest` system
- `agentos-kernel/src/tool_registry.rs` manages tool registration
- No connector abstraction exists

## Target State

- New `agentos-connectors` crate with `ConnectorDefinition` trait
- `ConnectorProxy` that intercepts namespaced tool calls and injects credentials
- `ConnectorRegistry` in the kernel managing active connectors
- Connectors can be defined as TOML manifests (like tools) or WASM/MCP plugins
- Agent sees `github.create_issue` as a regular tool — no awareness of HTTP details

---

## Detailed Subtasks

### 1. Create `agentos-connectors` crate

**File:** `crates/agentos-connectors/Cargo.toml`

```toml
[package]
name = "agentos-connectors"
version = "0.1.0"
edition = "2021"

[dependencies]
agentos-types = { path = "../agentos-types" }
agentos-vault = { path = "../agentos-vault" }
async-trait = "0.1"
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
```

Add to workspace `Cargo.toml` members list.

### 2. Define `ConnectorDefinition` trait

**File:** `crates/agentos-connectors/src/definition.rs`

```rust
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Defines a connector to an external service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorManifest {
    pub id: String,                    // "github", "slack", "stripe"
    pub name: String,                  // "GitHub"
    pub version: String,
    pub description: String,
    pub auth_type: AuthType,
    pub base_url: String,              // "https://api.github.com"
    pub tools: Vec<ConnectorToolDef>,  // tools this connector provides
    pub rate_limit: Option<RateLimitConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthType {
    Bearer,                            // Authorization: Bearer <token>
    OAuth2 { scopes: Vec<String> },    // OAuth2 with specific scopes
    ApiKey { header: String },         // Custom header (e.g., X-API-Key)
    Basic,                             // HTTP Basic auth
    None,                              // Public API
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorToolDef {
    pub name: String,                  // "create_issue" (namespaced as "github.create_issue")
    pub description: String,
    pub method: HttpMethod,
    pub path: String,                  // "/repos/{repo}/issues" (templated)
    pub input_schema: serde_json::Value, // JSON Schema for agent input
    pub response_map: Option<ResponseMap>, // Extract specific fields from response
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HttpMethod { Get, Post, Put, Patch, Delete }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
    pub burst: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMap {
    pub fields: Vec<ResponseField>,    // which JSON fields to extract
    pub max_body_bytes: usize,         // truncate large responses (default 32KB)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseField {
    pub source: String,                // JSON pointer: "/id", "/html_url"
    pub rename: Option<String>,        // optional rename in output
}
```

### 3. Implement `ConnectorProxy`

**File:** `crates/agentos-connectors/src/proxy.rs`

```rust
pub struct ConnectorProxy {
    manifest: ConnectorManifest,
    vault: Arc<SecretsVault>,
    http_client: reqwest::Client,
    rate_limiter: Option<RateLimiter>,
}

impl ConnectorProxy {
    pub fn new(manifest: ConnectorManifest, vault: Arc<SecretsVault>) -> Self;

    /// Execute a connector tool call
    /// 1. Look up the tool def by name
    /// 2. Template the URL path with input params
    /// 3. Fetch credentials from vault (static secret or OAuth)
    /// 4. Inject auth header
    /// 5. Make HTTP request
    /// 6. Apply response_map to extract relevant fields
    /// 7. Return JSON result (never includes raw auth headers)
    pub async fn execute(
        &self,
        tool_name: &str,    // "create_issue" (without namespace prefix)
        input: serde_json::Value,
    ) -> Result<serde_json::Value, AgentOSError>;
}
```

The proxy must:
- Never return raw response headers (could leak tokens in redirects)
- Respect rate limits (429 → wait and retry up to 3 times)
- Truncate response bodies to `max_body_bytes` (prevent context flooding)
- Log all requests to audit (connector_id, tool_name, status_code — not request/response bodies)

### 4. Implement `ConnectorRegistry`

**File:** `crates/agentos-connectors/src/registry.rs`

```rust
pub struct ConnectorRegistry {
    connectors: RwLock<HashMap<String, Arc<ConnectorProxy>>>,
    vault: Arc<SecretsVault>,
}

impl ConnectorRegistry {
    pub fn new(vault: Arc<SecretsVault>) -> Self;

    /// Register a connector from its manifest
    pub async fn register(&self, manifest: ConnectorManifest) -> Result<(), AgentOSError>;

    /// Unregister a connector
    pub fn deregister(&self, connector_id: &str) -> Result<(), AgentOSError>;

    /// List registered connectors (metadata only)
    pub fn list(&self) -> Vec<ConnectorManifest>;

    /// Route a namespaced tool call (e.g., "github.create_issue")
    /// Returns None if no connector matches the namespace
    pub async fn route(
        &self,
        namespaced_tool: &str,
        input: serde_json::Value,
    ) -> Option<Result<serde_json::Value, AgentOSError>>;
}
```

### 5. Kernel integration

**File:** `crates/agentos-kernel/src/kernel.rs`

Add `ConnectorRegistry` to the `Kernel` struct:

```rust
pub struct Kernel {
    // ... existing fields ...
    pub connector_registry: Arc<ConnectorRegistry>,
}
```

**File:** `crates/agentos-kernel/src/task_executor.rs`

In the tool execution path, before looking up in ToolRegistry, check if the tool name contains a dot (`.`) and try routing through ConnectorRegistry first:

```rust
// In execute_tool_call or similar:
if tool_name.contains('.') {
    if let Some(result) = self.connector_registry.route(tool_name, input).await {
        return result;
    }
}
// Fall through to normal tool registry lookup
```

### 6. Connector manifest loading

**File:** `crates/agentos-connectors/src/loader.rs`

Load connector manifests from `connectors/` directory (TOML format, similar to tool manifests):

```rust
pub fn load_connector_manifests(dir: &Path) -> Result<Vec<ConnectorManifest>, AgentOSError>;
```

Create `connectors/github.toml` as a reference connector:

```toml
[connector]
id = "github"
name = "GitHub"
version = "1.0.0"
description = "GitHub API connector"
base_url = "https://api.github.com"

[connector.auth]
type = "oauth2"
scopes = ["repo", "read:org"]

[[tools]]
name = "list_repos"
description = "List repositories for the authenticated user"
method = "get"
path = "/user/repos"

[[tools]]
name = "create_issue"
description = "Create an issue in a repository"
method = "post"
path = "/repos/{owner}/{repo}/issues"
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-connectors/` | **New crate** — definition, proxy, registry, loader |
| `crates/agentos-connectors/src/lib.rs` | Re-exports |
| `crates/agentos-connectors/src/definition.rs` | `ConnectorManifest`, `AuthType`, `ConnectorToolDef` |
| `crates/agentos-connectors/src/proxy.rs` | `ConnectorProxy` — HTTP execution with auth injection |
| `crates/agentos-connectors/src/registry.rs` | `ConnectorRegistry` — manages active connectors |
| `crates/agentos-connectors/src/loader.rs` | TOML manifest loader |
| `crates/agentos-kernel/src/kernel.rs` | Add `connector_registry` field |
| `crates/agentos-kernel/src/task_executor.rs` | Route namespaced tool calls through connectors |
| `Cargo.toml` (workspace) | Add `agentos-connectors` to members |
| `connectors/github.toml` | **New** — reference GitHub connector manifest |

---

## Dependencies

- **Requires:** Phase 1 (OAuth Token Lifecycle) — connectors use `vault.get_oauth()` for credentials
- **Blocks:** Phase 3 (OAuth Web Flow)

---

## Test Plan

1. **Unit: manifest parsing** — Load `github.toml`, verify all fields deserialize correctly
2. **Unit: URL templating** — Verify `/repos/{owner}/{repo}/issues` with `{"owner":"foo","repo":"bar"}` produces `/repos/foo/bar/issues`
3. **Unit: response mapping** — Verify `ResponseMap` extracts specified JSON pointer fields
4. **Unit: auth injection** — Verify Bearer/ApiKey/Basic headers are correctly injected (mock vault)
5. **Integration: proxy execution** — Use `wiremock` to mock GitHub API, verify full request/response cycle
6. **Integration: registry routing** — Register a connector, call `route("github.create_issue", ...)`, verify it reaches the proxy
7. **Security: no credential leakage** — Verify response JSON never contains `Authorization` header values
8. **Security: rate limiting** — Verify 429 responses trigger backoff and retry

---

## Verification

```bash
cargo build -p agentos-connectors
cargo test -p agentos-connectors
cargo test -p agentos-kernel
cargo clippy -p agentos-connectors -p agentos-kernel -- -D warnings
cargo fmt --all -- --check
```
