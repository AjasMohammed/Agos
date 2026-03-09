# Plan 01 — `http-client` Tool

## Goal

Add an outbound HTTP tool that allows agents to make authenticated HTTP requests to external APIs. Credentials are fetched from the secrets vault — never hardcoded. The tool respects the `network.outbound:x` permission gate.

---

## Why This First

The `http-client` tool is the most-requested missing piece from the standard library. It unlocks a huge surface area: agents can now call external APIs (weather, Slack, GitHub, internal services), making AgentOS practically useful without writing any custom WASM tools.

---

## Dependencies

- `agentos-tools` (extend existing crate)
- `agentos-types`
- `agentos-vault` (for secret injection into headers)
- `reqwest` (already a workspace dep — used by LLM adapters)

No new workspace dependencies needed.

---

## Permission Gate

```
Permission required: network.outbound:x
```

An agent without this permission cannot use the tool — the kernel rejects the intent before it ever reaches the tool.

---

## Tool Manifest

```toml
# tools/core/http-client.toml

[manifest]
name        = "http-client"
version     = "1.0.0"
description = "Make outbound HTTP requests (GET, POST, PUT, DELETE). Credentials injected from the secrets vault."
author      = "agentos-core"

[capabilities_required]
permissions = ["network.outbound:x"]

[capabilities_provided]
outputs = ["http.response"]

[intent_schema]
input  = "HttpRequest"
output = "HttpResponse"

[sandbox]
network       = true
fs_write      = false
gpu           = false
max_memory_mb = 64
max_cpu_ms    = 15000
```

---

## Input / Output Schema

```rust
/// Input — the agent sends this JSON to the tool
pub struct HttpRequest {
    pub method: HttpMethod,          // GET | POST | PUT | PATCH | DELETE
    pub url: String,                 // Full URL, validated
    pub headers: HashMap<String, String>,  // Additional headers
    pub body: Option<serde_json::Value>,   // JSON body for POST/PUT
    pub timeout_ms: Option<u64>,     // Default: 10_000ms
    /// Inject a secret from vault as a header value.
    /// e.g. { "Authorization": "Bearer $SLACK_TOKEN" }
    pub secret_headers: HashMap<String, String>,
}

pub enum HttpMethod {
    Get, Post, Put, Patch, Delete, Head
}

/// Output — returned to the LLM
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: serde_json::Value,     // Parsed as JSON if Content-Type is JSON, else string
    pub latency_ms: u64,
}
```

### Secret Header Injection

The `secret_headers` field lets agents reference vault secrets without ever seeing the values:

```json
{
    "url": "https://slack.com/api/chat.postMessage",
    "method": "POST",
    "secret_headers": {
        "Authorization": "Bearer $SLACK_TOKEN"
    },
    "body": { "channel": "#ops", "text": "Deployment complete." }
}
```

The tool resolves `$SLACK_TOKEN` from the vault at execution time. The raw value is **never returned** to the LLM — it's only used within the HTTP call and zeroed after.

---

## Implementation Plan

### 1. New file: `crates/agentos-tools/src/http_client.rs`

```rust
use agentos_types::{AgentOSError, PermissionOp};
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use async_trait::async_trait;
use reqwest::{Client, Method};
use std::collections::HashMap;
use std::time::Duration;

pub struct HttpClientTool {
    client: Client,
}

impl HttpClientTool {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("AgentOS/1.0")
            .build()
            .expect("Failed to build HTTP client");
        Self { client }
    }
}

#[async_trait]
impl AgentTool for HttpClientTool {
    fn name(&self) -> &str { "http-client" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.outbound".into(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        // 1. Deserialize request
        // 2. Validate URL (no private IPs in production mode)
        // 3. Resolve $SECRET_NAME references via context.vault
        // 4. Build and fire reqwest request
        // 5. Return structured response
        // 6. Zero secret values from memory
        todo!()
    }
}
```

### 2. Register in `crates/agentos-tools/src/runner.rs`

```rust
// In ToolRunner::new():
runner.register(Box::new(HttpClientTool::new()));
```

### 3. Tool manifest: `tools/core/http-client.toml`

See the manifest above.

---

## Security Considerations

| Risk                                 | Mitigation                                                                                    |
| ------------------------------------ | --------------------------------------------------------------------------------------------- |
| SSRF (Server-Side Request Forgery)   | Block requests to `127.x`, `10.x`, `192.168.x`, `169.254.x`, `::1` by default                 |
| Secret leakage in logs               | Scrub vault-resolved values from all log lines; log only the secret name                      |
| Unbounded response size              | Cap response body at 10MB; return truncation warning                                          |
| Redirect following to internal hosts | Disable redirects or re-validate the redirect target                                          |
| Prompt injection via response        | Tool response is wrapped in `[TOOL_RESULT]` delimiters — standard kernel sanitization applies |

---

## Tests

```rust
#[tokio::test]
async fn test_get_request_returns_json() {
    // Mock server → return { "hello": "world" }
    // Call tool with { method: GET, url: mock_url }
    // Assert response.body == { "hello": "world" }
}

#[tokio::test]
async fn test_ssrf_localhost_blocked() {
    // Call tool with url = "http://127.0.0.1/admin"
    // Assert error: SsrfBlocked
}

#[tokio::test]
async fn test_secret_header_injected_not_returned() {
    // Call with secret_headers: { "Authorization": "Bearer $MY_TOKEN" }
    // Assert raw token value does NOT appear in the returned HttpResponse
}

#[tokio::test]
async fn test_timeout_respected() {
    // Mock server that delays 30s
    // Call with timeout_ms: 500
    // Assert error: Timeout
}
```

---

## Verification

```bash
# Register the tool (should appear immediately)
agentctl tool list | grep http-client

# Grant permission to an agent
agentctl perm grant analyst network.outbound:x

# Run a task that uses it
agentctl task run --agent analyst \
  "Fetch the content from https://httpbin.org/json and tell me what it says"
```
