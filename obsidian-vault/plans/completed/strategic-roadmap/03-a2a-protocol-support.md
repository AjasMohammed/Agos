---
title: "Phase 3: A2A Protocol Support"
tags:
  - strategy
  - a2a
  - protocol
  - interop
  - phase-3
date: 2026-04-08
status: planned
effort: 2w
priority: high
---

# Phase 3: A2A Protocol Support

> Implement Google's Agent-to-Agent (A2A) protocol to enable AgentOS agents to discover, negotiate with, and collaborate with agents running on external frameworks (LangGraph, PydanticAI, Google ADK).

---

## Why This Phase

Research finding: MCP handles agent↔tool communication. A2A handles **agent↔agent** communication. They are complementary. PydanticAI and Google ADK already support both. For AgentOS to be the "neutral ground" where diverse agent ecosystems collaborate, it needs A2A.

Without A2A, AgentOS agents can only talk to other AgentOS agents (via multi-agent coordination) or expose tools via MCP. With A2A, a LangGraph research agent can delegate secure file operations to an AgentOS agent, or a CrewAI team can include an AgentOS specialist.

**Strategic value:** A2A makes AgentOS agents first-class citizens in any multi-framework deployment.

---

## Current → Target State

**Current:** Multi-agent coordination exists within AgentOS (Phase 1-4 of multi-agent plan). No inter-framework agent communication. No A2A implementation.

**Target:** AgentOS agents can advertise capabilities via A2A Agent Cards, receive task delegations from external agents, and delegate tasks to external A2A-compliant agents. All interactions validated by CapabilityTokens and logged to audit.

---

## Detailed Subtasks

### 1. Implement A2A Agent Card

An Agent Card is a JSON document that describes an agent's capabilities, authentication requirements, and endpoint:

```rust
// crates/agentos-mcp/src/a2a/agent_card.rs
// (A2A lives in the MCP crate since it shares the protocol layer)

#[derive(Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,                    // Agent's A2A endpoint
    pub capabilities: Vec<AgentCapability>,
    pub authentication: AuthRequirement, // CapabilityToken, API key, etc.
    pub version: String,
    pub provider: String,               // "agentos"
}

#[derive(Serialize, Deserialize)]
pub struct AgentCapability {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
}
```

### 2. Implement A2A Task Protocol

The A2A task lifecycle: `submitted` → `working` → `completed` / `failed`

```rust
// crates/agentos-mcp/src/a2a/task.rs

#[derive(Serialize, Deserialize)]
pub struct A2ATask {
    pub id: String,
    pub sender: String,        // Requesting agent's card URL
    pub capability: String,    // Which capability to invoke
    pub input: serde_json::Value,
    pub status: A2ATaskStatus,
}

pub enum A2ATaskStatus {
    Submitted,
    Working,
    Completed { output: serde_json::Value },
    Failed { error: String },
}
```

### 3. A2A Server Endpoint

Expose A2A endpoints alongside MCP:

```
GET  /.well-known/agent.json    → Return this agent's AgentCard
POST /a2a/tasks                  → Receive task delegation
GET  /a2a/tasks/{id}             → Check task status
POST /a2a/tasks/{id}/cancel      → Cancel a running task
```

**Security enforcement:**
- Every incoming A2A task validated against CapabilityToken
- External agents must present a valid token or API key
- All A2A interactions logged to audit: `A2ATaskReceived`, `A2ATaskCompleted`, `A2ATaskRejected`

### 4. A2A Client (Outbound Delegation)

Allow AgentOS agents to discover and delegate to external A2A agents:

```rust
// crates/agentos-mcp/src/a2a/client.rs

pub struct A2AClient {
    http_client: reqwest::Client,
}

impl A2AClient {
    /// Discover an external agent's capabilities
    pub async fn discover(&self, agent_url: &str) -> Result<AgentCard>;

    /// Delegate a task to an external agent
    pub async fn delegate(&self, agent_url: &str, task: A2ATask) -> Result<A2ATaskStatus>;

    /// Poll task status
    pub async fn poll_status(&self, agent_url: &str, task_id: &str) -> Result<A2ATaskStatus>;
}
```

### 5. AgentOS Tool: `a2a-delegate`

A built-in tool that agents can use to delegate work to external A2A agents:

```toml
# tools/core/a2a-delegate.toml
[tool]
name = "a2a-delegate"
description = "Delegate a task to an external A2A-compatible agent"
trust_tier = "core"
permissions = ["network:outbound", "a2a:delegate"]
```

### 6. CLI Commands

```bash
# Show this agent's A2A card
agentos a2a card

# Discover an external agent
agentos a2a discover https://external-agent.example.com

# Delegate a task
agentos a2a delegate --agent https://external-agent.example.com --capability "research" --input '{"query": "..."}'

# List active A2A tasks
agentos a2a tasks
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-mcp/src/a2a/mod.rs` (new) | A2A module root |
| `crates/agentos-mcp/src/a2a/agent_card.rs` (new) | Agent Card definition |
| `crates/agentos-mcp/src/a2a/task.rs` (new) | Task protocol types |
| `crates/agentos-mcp/src/a2a/server.rs` (new) | A2A server endpoints |
| `crates/agentos-mcp/src/a2a/client.rs` (new) | Outbound A2A client |
| `crates/agentos-mcp/src/lib.rs` | Re-export A2A module |
| `tools/core/a2a-delegate.toml` (new) | Delegation tool manifest |
| `crates/agentos-tools/src/a2a_tools.rs` (new) | Tool implementation |
| `crates/agentos-cli/src/commands/a2a.rs` (new) | CLI subcommands |
| `crates/agentos-audit/src/log.rs` | Add A2A event types |
| `crates/agentos-mcp/tests/a2a_interop.rs` (new) | Interop tests |

---

## Dependencies

- **Requires:** Phase 2 (MCP transport layer and server infrastructure)
- **Blocks:** Phase 7 (orchestration bridges use A2A for framework interop)

---

## Test Plan

1. Unit: Agent Card serialization/deserialization roundtrip
2. Integration: Start A2A server → GET `/.well-known/agent.json` → valid card returned
3. Integration: Submit A2A task → agent processes → status transitions correctly
4. Security: A2A task without valid token → rejected + audit log entry
5. Interop: Mock external A2A agent → AgentOS client discovers and delegates successfully
6. Full: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`

---

## Verification

```bash
# Build
cargo build -p agentos-mcp

# Run A2A tests
cargo test -p agentos-mcp -- a2a

# Test card endpoint
curl http://localhost:3001/.well-known/agent.json | jq .

# Full workspace check
cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings
```
