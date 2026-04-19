---
title: "Phase 2: MCP-Native Secure Router"
tags:
  - strategy
  - mcp
  - protocol
  - phase-2
date: 2026-04-08
status: planned
effort: 3w
priority: critical
---

# Phase 2: MCP-Native Secure Router

> Complete the `agentos-mcp` crate to implement the full MCP specification, layer CapabilityToken validation on every MCP tool call, and support all three standard transports. Position AgentOS as the fastest, safest Rust-native MCP router.

---

## Why This Phase

Research finding: MCP (Model Context Protocol) has become the universal standard for agent↔tool communication. Adopted by Anthropic, OpenAI, Google, and Microsoft. The mcp-agent project won mindshare by being **MCP-first** — built on the protocol from day one, not adapter-bolted-on.

AgentOS already has `crates/agentos-mcp/` but it needs full spec completion. The strategic opportunity: no existing MCP implementation validates **capability tokens** on every tool call or sandboxes tool execution. AgentOS can be the only MCP router that provides kernel-level security enforcement.

**Positioning line:** "The only MCP router that validates capability tokens and sandboxes execution."

---

## Current → Target State

**Current:** `agentos-mcp` crate exists but needs assessment for spec completeness. AgentOS tools are accessible via internal `KernelCommand` dispatch but not yet exposed as MCP servers.

**Target:** Full MCP spec compliance — tools, resources, prompts, notifications, OAuth, sampling — over all three transports (Stdio, SSE, Streamable HTTP), with CapabilityToken validation on every incoming request.

---

## Detailed Subtasks

### 1. Audit Current `agentos-mcp` State

Read `crates/agentos-mcp/src/` to determine:
- Which MCP primitives are implemented (tools, resources, prompts, notifications, sampling)
- Which transports are supported (Stdio, SSE, Streamable HTTP)
- How tool calls are dispatched (direct kernel, or adapter)
- Whether capability validation exists on MCP routes

### 2. Implement Full MCP Spec Primitives

Based on the MCP specification (JSON-RPC 2.0):

**Tools** — expose AgentOS tools as MCP tools:
```rust
// Each AgentOS ToolManifest maps to an MCP Tool definition
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value, // JSON Schema
}

// Tool call flow:
// MCP Request → Parse → Validate CapabilityToken → Dispatch to kernel → Return result
```

**Resources** — expose kernel state as read-only MCP resources:
- Agent list, task status, audit log entries, memory queries
- Each resource requires a read CapabilityToken

**Prompts** — expose AgentOS skill manifests as MCP prompts:
- Map `SKILL.toml` manifests to MCP prompt templates
- Include parameter schemas from skill definitions

**Notifications** — emit kernel events as MCP notifications:
- Task completion, escalation created, security rejection, agent state change
- Wire to existing `agentos-bus` event stream

**Sampling** — allow MCP clients to request LLM inference through AgentOS:
- Route to `agentos-llm` adapter layer
- Enforce cost tracking and budget limits on sampled inferences

### 3. Implement All Three Transports

**Stdio** — for local CLI/pipe integration:
```rust
// Read JSON-RPC from stdin, write to stdout
// Used by: local development, IDE extensions, Claude Code
pub struct StdioTransport;
```

**SSE (Server-Sent Events)** — for browser/web clients:
```rust
// HTTP endpoint that streams MCP notifications
// Pairs with POST endpoint for requests
// Used by: web UI, dashboard, remote monitoring
pub struct SseTransport;
```

**Streamable HTTP** — for production API integration:
```rust
// Standard HTTP POST for request/response
// Streaming response body for long operations
// Used by: orchestrators (LangGraph, CrewAI), CI/CD
pub struct StreamableHttpTransport;
```

### 4. CapabilityToken Validation Layer

Every MCP request must include a capability token. The MCP router validates before dispatching:

```rust
pub async fn handle_mcp_request(
    req: McpRequest,
    token: CapabilityToken,
    kernel: &Kernel,
) -> McpResponse {
    // 1. Validate token signature (HMAC-SHA256)
    // 2. Check token permissions against requested MCP primitive
    // 3. Check token expiry
    // 4. Log to audit: McpRequestReceived { tool, token_id, result }
    // 5. Dispatch to kernel
    // 6. Return McpResponse
}
```

**Permission mapping:**
| MCP Primitive | Required Permission |
|--------------|-------------------|
| `tools/call` | Tool-specific permission from manifest |
| `resources/read` | `mcp:resource:read` |
| `prompts/get` | `mcp:prompt:read` |
| `sampling/create` | `llm:inference` + cost budget check |

### 5. MCP Server CLI Commands

```bash
# Start MCP server on stdio
agentos mcp serve --transport stdio

# Start MCP server on SSE (port 3001)
agentos mcp serve --transport sse --port 3001

# Start MCP server on HTTP (port 3002)
agentos mcp serve --transport http --port 3002

# List available MCP tools
agentos mcp tools

# Test a tool call with capability token
agentos mcp call --tool "file-read" --input '{"path": "/tmp/test.txt"}' --token <TOKEN>
```

### 6. MCP Compliance Test Suite

```rust
// crates/agentos-mcp/tests/spec_compliance.rs

#[tokio::test]
async fn test_tools_list_returns_all_registered_tools() { ... }

#[tokio::test]
async fn test_tool_call_validates_capability_token() { ... }

#[tokio::test]
async fn test_tool_call_rejects_expired_token() { ... }

#[tokio::test]
async fn test_resources_list_exposes_kernel_state() { ... }

#[tokio::test]
async fn test_prompts_map_from_skill_manifests() { ... }

#[tokio::test]
async fn test_sampling_enforces_cost_budget() { ... }

#[tokio::test]
async fn test_stdio_transport_roundtrip() { ... }

#[tokio::test]
async fn test_sse_transport_notifications() { ... }

#[tokio::test]
async fn test_http_transport_streaming() { ... }

#[tokio::test]
async fn test_unauthorized_request_rejected_and_audited() { ... }
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-mcp/src/lib.rs` | Re-export public API |
| `crates/agentos-mcp/src/server.rs` | MCP server core with token validation |
| `crates/agentos-mcp/src/tools.rs` | Tool primitive (list, call) |
| `crates/agentos-mcp/src/resources.rs` | Resource primitive (list, read) |
| `crates/agentos-mcp/src/prompts.rs` | Prompt primitive (list, get) |
| `crates/agentos-mcp/src/notifications.rs` | Notification emitter |
| `crates/agentos-mcp/src/sampling.rs` | Sampling primitive with cost tracking |
| `crates/agentos-mcp/src/transport/mod.rs` | Transport trait |
| `crates/agentos-mcp/src/transport/stdio.rs` | Stdio transport |
| `crates/agentos-mcp/src/transport/sse.rs` | SSE transport |
| `crates/agentos-mcp/src/transport/http.rs` | Streamable HTTP transport |
| `crates/agentos-cli/src/commands/mcp.rs` | CLI subcommands for MCP |
| `crates/agentos-mcp/tests/spec_compliance.rs` (new) | Compliance tests |

---

## Dependencies

- **Requires:** Nothing — can start immediately (parallel with Phase 1)
- **Blocks:** Phase 3 (A2A builds on MCP transport layer), Phase 5 (enterprise features need MCP surface), Phase 6 (marketplace tools exposed via MCP), Phase 7 (orchestration bridges use MCP)

---

## Test Plan

1. `cargo test -p agentos-mcp` — all spec compliance tests pass
2. Manual: `agentos mcp serve --transport stdio` → pipe JSON-RPC request → correct response
3. Manual: `agentos mcp serve --transport sse` → curl SSE endpoint → receive notifications
4. Token rejection: send request without token → 401 + audit log entry
5. Cost enforcement: sampling request exceeding budget → rejected
6. Full workspace: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`

---

## Verification

```bash
# Build MCP crate
cargo build -p agentos-mcp

# Run compliance tests
cargo test -p agentos-mcp -- spec_compliance

# Start stdio server and test
echo '{"jsonrpc":"2.0","method":"tools/list","id":1}' | cargo run -- mcp serve --transport stdio

# Full workspace check
cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings
```
