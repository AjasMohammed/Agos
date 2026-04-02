# Agentic Event Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give agents five tools (`event-subscribe`, `event-unsubscribe`, `event-list-subscriptions`, `event-emit`, `event-history`) to manage event subscriptions and emit custom events at runtime, enabling pure agentic reactive workflows with zero human intervention.

**Architecture:** Stateless tools return `_kernel_action` JSON. The kernel intercepts via `dispatch_kernel_action()` and performs the privileged operation — same pattern as `agent-message`, `task-delegate`, `agent-call`. Custom events use a new `EventType::Custom(String)` variant. Per-agent rate limiting prevents event flooding.

**Tech Stack:** Rust, async-trait, serde_json, tokio, chrono

**Spec:** `docs/superpowers/specs/2026-03-30-agentic-event-tools-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/agentos-types/src/event.rs` | Modify | Add `Custom(String)` to `EventType`, `AgentDefined` to `EventCategory`, `Agent(AgentID)` to `EventSource` |
| `crates/agentos-kernel/src/event_bus.rs` | Modify | Extend `parse_event_type_filter()` for `"Custom:..."` syntax, extend `parse_event_category()` |
| `crates/agentos-kernel/src/kernel_action.rs` | Modify | Add 5 `KernelAction` variants, `from_tool_result()` arms, `dispatch_kernel_action()` handlers |
| `crates/agentos-kernel/src/kernel.rs` | Modify | Add `agent_event_rate` field + `RateWindow` struct |
| `crates/agentos-kernel/src/commands/agent.rs` | Modify | Grant `event.self:rw` and `event.manage:rw` permissions at connect |
| `crates/agentos-tools/src/event_subscribe.rs` | Create | `EventSubscribeTool` — stateless `_kernel_action` stub |
| `crates/agentos-tools/src/event_unsubscribe.rs` | Create | `EventUnsubscribeTool` — stateless `_kernel_action` stub |
| `crates/agentos-tools/src/event_list_subscriptions.rs` | Create | `EventListSubscriptionsTool` — stateless `_kernel_action` stub |
| `crates/agentos-tools/src/event_emit.rs` | Create | `EventEmitTool` — stateless `_kernel_action` stub |
| `crates/agentos-tools/src/event_history.rs` | Create | `EventHistoryTool` — stateless `_kernel_action` stub |
| `crates/agentos-tools/src/lib.rs` | Modify | Add `mod` + `pub use` for 5 new modules |
| `crates/agentos-tools/src/runner.rs` | Modify | Register 5 new tools in `register_memory_tools()` |
| `tools/core/event-subscribe.toml` | Create | Tool manifest |
| `tools/core/event-unsubscribe.toml` | Create | Tool manifest |
| `tools/core/event-list-subscriptions.toml` | Create | Tool manifest |
| `tools/core/event-emit.toml` | Create | Tool manifest |
| `tools/core/event-history.toml` | Create | Tool manifest |
| `config/default.toml` | Modify | Add `agent_emit_rate_limit` to `[kernel.events]` |

---

### Task 1: Type System — `EventType::Custom`, `EventCategory::AgentDefined`, `EventSource::Agent`

**Files:**
- Modify: `crates/agentos-types/src/event.rs`

This task adds the foundational type changes that all other tasks depend on.

- [ ] **Step 1: Add `Custom(String)` variant to `EventType` enum**

Open `crates/agentos-types/src/event.rs`. The `EventType` enum ends at line 122. The last variant is `ExternalAlertReceived` at line 121. Add the new variant before the closing brace:

```rust
// In EventType enum, after line 121 (ExternalAlertReceived,):
    // ── Agent-Defined Custom Events ──
    /// Agent-defined custom event type. The string is the event name
    /// chosen by the emitting agent (e.g., "DataPipelineComplete").
    Custom(String),
```

- [ ] **Step 2: Add `AgentDefined` variant to `EventCategory` enum**

The `EventCategory` enum is at lines 9-20. Add after `ExternalEvents`:

```rust
// After ExternalEvents in EventCategory enum:
    /// Events defined and emitted by agents at runtime.
    AgentDefined,
```

- [ ] **Step 3: Update `category()` method**

The `category()` method is at lines 124-211. The match statement ends with the `ExternalEvents` arm at lines 205-208, then the closing brace at line 209. Add a new arm before the closing brace:

```rust
// After the ExternalEvents arm (line 208), before the closing brace:
            Self::Custom(_) => EventCategory::AgentDefined,
```

- [ ] **Step 4: Add `Agent(AgentID)` variant to `EventSource` enum**

The `EventSource` enum is at lines 228-242. It has `#[derive(Debug, Clone, Copy, ...)]` — but `AgentID` is not `Copy`. Change `Copy` to nothing or check if AgentID is Copy. Since `AgentID` wraps a `Uuid` which IS `Copy`, this should work. Add after `ExternalBridge`:

```rust
// After ExternalBridge in EventSource enum:
    /// Event emitted by an agent via the event-emit tool.
    Agent(AgentID),
```

Note: You need to ensure `AgentID` is imported. Check the top of the file — `agentos_types` types are in the same crate, so `use crate::ids::AgentID;` or it may already be available via `use crate::*;`. Check existing imports at the top of `event.rs`.

- [ ] **Step 5: Build to verify type changes compile**

Run: `cargo build -p agentos-types 2>&1 | head -30`

Expected: Build succeeds, OR you get exhaustive match errors in other crates (that's expected — those crates will be fixed in later tasks). The `agentos-types` crate itself must compile.

- [ ] **Step 6: Fix exhaustive match warnings in the types crate**

The `Display` impl for `EventType` at line 213-217 uses `{:?}` which handles `Custom(String)` automatically via `Debug`. The `Display` impl for `EventCategory` at lines 219-223 also uses `{:?}`. No changes needed for these.

However, check if there are any `match` statements in this file that enumerate all variants. If so, add `Custom(_)` and `AgentDefined` arms. The `category()` match was already handled in Step 3.

- [ ] **Step 7: Build the full workspace to find all exhaustive match breakages**

Run: `cargo build --workspace 2>&1 | head -60`

Expected: Other crates (kernel, etc.) will have exhaustive match errors on `EventType`, `EventCategory`, and `EventSource`. Note which files need fixes — they'll be addressed in Task 2.

- [ ] **Step 8: Commit**

```bash
git add crates/agentos-types/src/event.rs
git commit -m "feat(types): add Custom event type, AgentDefined category, and Agent event source"
```

---

### Task 2: Fix Exhaustive Match Breakages Across Workspace

**Files:**
- Modify: Any files that broke from Task 1's type changes

After Task 1, `cargo build --workspace` will show exhaustive match errors. This task fixes all of them.

- [ ] **Step 1: Identify all broken matches**

Run: `cargo build --workspace 2>&1 | grep "non-exhaustive"`

This will list every file and line that has an exhaustive match on `EventType`, `EventCategory`, or `EventSource` that doesn't handle the new variants.

- [ ] **Step 2: Fix each broken match**

For each file:

**`EventType` matches** — add a `Custom(_)` arm. The behavior depends on context:
- In `category()`: already handled (Task 1, Step 3)
- In `trigger_prompt.rs` (the `build_trigger_prompt` function): add a generic arm for `Custom(name)` that builds a prompt like: `"Custom event '{name}' was emitted. Payload: {payload}"`
- In any serde/display code: `Custom(name)` should display as `"Custom({name})"`
- In `parse_event_type()` in `event_bus.rs`: this returns `Option<EventType>` for known names — add a fallback: if the name starts with `"Custom:"`, parse it; otherwise return `None` (existing behavior for unknown names)

**`EventCategory` matches** — add an `AgentDefined` arm:
- In `parse_event_category()` in `event_bus.rs`: add `"agentdefined" | "agent_defined" | "agent-defined" => Some(EventCategory::AgentDefined)`

**`EventSource` matches** — add an `Agent(_)` arm:
- Any display/serialization code should handle `Agent(id)` as `"Agent({id})"`

- [ ] **Step 3: Build the workspace**

Run: `cargo build --workspace 2>&1 | head -30`
Expected: Clean build with no errors.

- [ ] **Step 4: Run all tests**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: All existing tests pass. No regressions.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "fix: handle Custom/AgentDefined/Agent variants in all exhaustive matches"
```

---

### Task 3: Extend `parse_event_type_filter()` for `"Custom:..."` Syntax

**Files:**
- Modify: `crates/agentos-kernel/src/event_bus.rs`

- [ ] **Step 1: Write test for Custom event filter parsing**

Add a test to the existing `#[cfg(test)]` module in `event_bus.rs`:

```rust
#[test]
fn parse_custom_event_filter() {
    // "Custom:MyEvent" → Exact(Custom("MyEvent"))
    let filter = parse_event_type_filter("Custom:DataPipelineComplete");
    assert_eq!(
        filter,
        Some(EventTypeFilter::Exact(EventType::Custom(
            "DataPipelineComplete".into()
        )))
    );

    // "Custom:" with empty name → None
    let empty = parse_event_type_filter("Custom:");
    assert!(empty.is_none());

    // "category:AgentDefined" → Category(AgentDefined)
    let cat = parse_event_type_filter("category:AgentDefined");
    assert_eq!(
        cat,
        Some(EventTypeFilter::Category(EventCategory::AgentDefined))
    );

    // "AgentDefined.*" → Category(AgentDefined)
    let cat_star = parse_event_type_filter("AgentDefined.*");
    assert_eq!(
        cat_star,
        Some(EventTypeFilter::Category(EventCategory::AgentDefined))
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p agentos-kernel parse_custom_event_filter -- --nocapture 2>&1 | tail -10`
Expected: FAIL — `parse_event_type_filter("Custom:DataPipelineComplete")` returns `None`.

- [ ] **Step 3: Implement `Custom:` prefix handling**

In `parse_event_type_filter()` (line 303 of `event_bus.rs`), add a new check after the `category:` prefix check (around line 318) and before the `.*` suffix check:

```rust
    // Handle "Custom:<EventName>" → Exact(Custom("<EventName>"))
    if let Some(custom_name) = trimmed.strip_prefix("Custom:") {
        let name = custom_name.trim();
        if name.is_empty() {
            return None;
        }
        return Some(EventTypeFilter::Exact(EventType::Custom(name.to_string())));
    }
```

- [ ] **Step 4: Ensure `parse_event_category()` handles `AgentDefined`**

Find `parse_event_category()` in `event_bus.rs` (it should be near `parse_event_type_filter`). Add an arm for the new category:

```rust
"agentdefined" | "agent_defined" | "agent-defined" => Some(EventCategory::AgentDefined),
```

This makes `"category:AgentDefined"` and `"AgentDefined.*"` work automatically.

- [ ] **Step 5: Run the test**

Run: `cargo test -p agentos-kernel parse_custom_event_filter -- --nocapture 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 6: Run all kernel tests**

Run: `cargo test -p agentos-kernel 2>&1 | tail -20`
Expected: All pass.

- [ ] **Step 7: Commit**

```bash
git add crates/agentos-kernel/src/event_bus.rs
git commit -m "feat(event-bus): support Custom: prefix in event type filter parsing"
```

---

### Task 4: Per-Agent Emit Rate Limiting on Kernel

**Files:**
- Modify: `crates/agentos-kernel/src/kernel.rs`
- Modify: `config/default.toml`

- [ ] **Step 1: Add `agent_emit_rate_limit` to config**

Open `config/default.toml`. The `[kernel.events]` section is at lines 37-42. Add the new field after line 42 (`channel_capacity = 1024`):

```toml
# Maximum custom events per second per agent via the event-emit tool.
# Set to 0 for unlimited (useful for testing).
agent_emit_rate_limit = 10
```

- [ ] **Step 2: Add config field to `KernelConfig`**

Find the `KernelConfig` struct (or the nested `EventsConfig` sub-struct) that deserializes `[kernel.events]`. Add:

```rust
/// Max custom events per second per agent. 0 = unlimited.
#[serde(default = "default_agent_emit_rate_limit")]
pub agent_emit_rate_limit: u32,
```

And the default function:

```rust
fn default_agent_emit_rate_limit() -> u32 {
    10
}
```

- [ ] **Step 3: Add `RateWindow` struct and `agent_event_rate` field to Kernel**

In `crates/agentos-kernel/src/kernel.rs`, add the `RateWindow` struct near the top of the file (before the `Kernel` struct):

```rust
/// Sliding-window rate limiter state for per-agent event emission.
struct RateWindow {
    count: u32,
    window_start: chrono::DateTime<chrono::Utc>,
}
```

Add the field to the `Kernel` struct (after `task_scoped_subscriptions` at line 369):

```rust
    /// Per-agent rate limiter for custom event emission via the event-emit tool.
    pub(crate) agent_event_rate: Arc<RwLock<HashMap<AgentID, RateWindow>>>,
```

- [ ] **Step 4: Initialize the field in the Kernel constructor**

Find where `task_scoped_subscriptions` is initialized (line 2087). Add nearby:

```rust
            agent_event_rate: Arc::new(RwLock::new(HashMap::new())),
```

- [ ] **Step 5: Add a helper method for rate checking**

Add a method on `Kernel`:

```rust
    /// Check and update the per-agent event emission rate limiter.
    /// Returns `true` if the agent is within the rate limit, `false` if exceeded.
    pub(crate) async fn check_agent_event_rate(&self, agent_id: &AgentID) -> bool {
        let limit = self.config.events.agent_emit_rate_limit;
        if limit == 0 {
            return true; // unlimited
        }

        let now = chrono::Utc::now();
        let mut rates = self.agent_event_rate.write().await;
        let window = rates.entry(*agent_id).or_insert(RateWindow {
            count: 0,
            window_start: now,
        });

        let elapsed = now.signed_duration_since(window.window_start);
        if elapsed.num_seconds() >= 1 {
            // Reset window
            window.count = 1;
            window.window_start = now;
            true
        } else if window.count < limit {
            window.count += 1;
            true
        } else {
            false // rate exceeded
        }
    }
```

- [ ] **Step 6: Build**

Run: `cargo build -p agentos-kernel 2>&1 | head -20`
Expected: Clean build.

- [ ] **Step 7: Commit**

```bash
git add config/default.toml crates/agentos-kernel/src/kernel.rs
git commit -m "feat(kernel): add per-agent event emission rate limiter"
```

---

### Task 5: Default Permission Grants (`event.self:rw`, `event.manage:rw`)

**Files:**
- Modify: `crates/agentos-kernel/src/commands/agent.rs`

- [ ] **Step 1: Add `event.self` permission to `default_permissions_for_agent()`**

Open `crates/agentos-kernel/src/commands/agent.rs`. The `default_permissions_for_agent()` function is at line 852. Find line 896 where `events.stream` is granted:

```rust
    // Event stream — observe (subscribe/unsubscribe to kernel events)
    perms.grant_op("events.stream".to_string(), PermissionOp::Observe, None);
```

Add the new `event.self` permission right after it:

```rust
    // Event self-management — agents can manage their own event subscriptions and emit custom events
    perms.grant("event.self".to_string(), true, true, false, None);
```

- [ ] **Step 2: Add `event.manage` permission for orchestrator role**

Find where role-based extra permissions are applied in `cmd_connect_agent()`. The role-based default subscriptions are applied at lines 412-434. Look for where orchestrator-specific permissions might already be set. If there's no role-based permission block, add one just before the default subscription block (before line 412):

```rust
        // Grant event.manage permission to orchestrator agents.
        if profile.roles.iter().any(|r| r.eq_ignore_ascii_case("orchestrator")) {
            profile.permissions.grant("event.manage".to_string(), true, true, false, None);
        }
```

If permissions are stored via `persisted_permissions` or `profile_manager`, follow the existing pattern for how permissions are persisted. The key is that the `PermissionSet` used for the agent's tasks includes `event.manage:rw` when the agent has the `orchestrator` role.

- [ ] **Step 3: Build**

Run: `cargo build -p agentos-kernel 2>&1 | head -20`
Expected: Clean build.

- [ ] **Step 4: Run tests**

Run: `cargo test -p agentos-kernel 2>&1 | tail -20`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-kernel/src/commands/agent.rs
git commit -m "feat(agent): grant event.self and event.manage permissions at connect"
```

---

### Task 6: Tool Implementations — 5 Stateless `_kernel_action` Stubs

**Files:**
- Create: `crates/agentos-tools/src/event_subscribe.rs`
- Create: `crates/agentos-tools/src/event_unsubscribe.rs`
- Create: `crates/agentos-tools/src/event_list_subscriptions.rs`
- Create: `crates/agentos-tools/src/event_emit.rs`
- Create: `crates/agentos-tools/src/event_history.rs`
- Modify: `crates/agentos-tools/src/lib.rs`
- Modify: `crates/agentos-tools/src/runner.rs`

All five tools follow the exact same pattern as `agent_message.rs` (line 1-51): validate payload fields, return `_kernel_action` JSON.

- [ ] **Step 1: Create `event_subscribe.rs`**

Create `crates/agentos-tools/src/event_subscribe.rs`:

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct EventSubscribeTool;

impl EventSubscribeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventSubscribeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventSubscribeTool {
    fn name(&self) -> &str {
        "event-subscribe"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("event.self".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let event_filter = payload
            .get("event_filter")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "event-subscribe requires 'event_filter' field".into(),
                )
            })?;

        let target_agent = payload
            .get("target_agent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let payload_filter = payload
            .get("payload_filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let throttle = payload
            .get("throttle")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let priority = payload
            .get("priority")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let scope = payload
            .get("scope")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(serde_json::json!({
            "_kernel_action": "event_subscribe",
            "event_filter": event_filter,
            "target_agent": target_agent,
            "payload_filter": payload_filter,
            "throttle": throttle,
            "priority": priority,
            "scope": scope,
        }))
    }
}
```

- [ ] **Step 2: Create `event_unsubscribe.rs`**

Create `crates/agentos-tools/src/event_unsubscribe.rs`:

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct EventUnsubscribeTool;

impl EventUnsubscribeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventUnsubscribeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventUnsubscribeTool {
    fn name(&self) -> &str {
        "event-unsubscribe"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("event.self".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let subscription_id = payload
            .get("subscription_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "event-unsubscribe requires 'subscription_id' field".into(),
                )
            })?;

        Ok(serde_json::json!({
            "_kernel_action": "event_unsubscribe",
            "subscription_id": subscription_id,
        }))
    }
}
```

- [ ] **Step 3: Create `event_list_subscriptions.rs`**

Create `crates/agentos-tools/src/event_list_subscriptions.rs`:

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct EventListSubscriptionsTool;

impl EventListSubscriptionsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventListSubscriptionsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventListSubscriptionsTool {
    fn name(&self) -> &str {
        "event-list-subscriptions"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("event.self".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let target_agent = payload
            .get("target_agent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(serde_json::json!({
            "_kernel_action": "event_list_subscriptions",
            "target_agent": target_agent,
        }))
    }
}
```

- [ ] **Step 4: Create `event_emit.rs`**

Create `crates/agentos-tools/src/event_emit.rs`:

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct EventEmitTool;

impl EventEmitTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventEmitTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventEmitTool {
    fn name(&self) -> &str {
        "event-emit"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("event.self".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let event_type = payload
            .get("event_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "event-emit requires 'event_type' field".into(),
                )
            })?;

        let event_payload = payload
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let severity = payload
            .get("severity")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(serde_json::json!({
            "_kernel_action": "event_emit",
            "event_type": event_type,
            "payload": event_payload,
            "severity": severity,
        }))
    }
}
```

- [ ] **Step 5: Create `event_history.rs`**

Create `crates/agentos-tools/src/event_history.rs`:

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct EventHistoryTool;

impl EventHistoryTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventHistoryTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for EventHistoryTool {
    fn name(&self) -> &str {
        "event-history"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("event.self".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let last = payload
            .get("last")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as u32;

        Ok(serde_json::json!({
            "_kernel_action": "event_history",
            "last": last,
        }))
    }
}
```

- [ ] **Step 6: Add modules and re-exports to `lib.rs`**

Open `crates/agentos-tools/src/lib.rs`. Add module declarations in alphabetical order (after `episodic_list` at line 13, before `escalation_status` at line 14):

```rust
pub mod event_emit;
pub mod event_history;
pub mod event_list_subscriptions;
pub mod event_subscribe;
pub mod event_unsubscribe;
```

Add re-exports in the `pub use` block (after the `EpisodicList` re-export, keeping alphabetical order):

```rust
pub use event_emit::EventEmitTool;
pub use event_history::EventHistoryTool;
pub use event_list_subscriptions::EventListSubscriptionsTool;
pub use event_subscribe::EventSubscribeTool;
pub use event_unsubscribe::EventUnsubscribeTool;
```

- [ ] **Step 7: Register tools in `runner.rs`**

Open `crates/agentos-tools/src/runner.rs`. In the `register_memory_tools()` method, add after the existing agent/task tools (around line 186, after `AgentCallTool`):

```rust
        // Event management tools
        runner.register(Box::new(crate::event_subscribe::EventSubscribeTool::new()));
        runner.register(Box::new(crate::event_unsubscribe::EventUnsubscribeTool::new()));
        runner.register(Box::new(crate::event_list_subscriptions::EventListSubscriptionsTool::new()));
        runner.register(Box::new(crate::event_emit::EventEmitTool::new()));
        runner.register(Box::new(crate::event_history::EventHistoryTool::new()));
```

- [ ] **Step 8: Build**

Run: `cargo build -p agentos-tools 2>&1 | head -20`
Expected: Clean build.

- [ ] **Step 9: Commit**

```bash
git add crates/agentos-tools/src/event_subscribe.rs \
       crates/agentos-tools/src/event_unsubscribe.rs \
       crates/agentos-tools/src/event_list_subscriptions.rs \
       crates/agentos-tools/src/event_emit.rs \
       crates/agentos-tools/src/event_history.rs \
       crates/agentos-tools/src/lib.rs \
       crates/agentos-tools/src/runner.rs
git commit -m "feat(tools): add 5 event management tools (subscribe, unsubscribe, list, emit, history)"
```

---

### Task 7: Tool Manifests — 5 TOML Files in `tools/core/`

**Files:**
- Create: `tools/core/event-subscribe.toml`
- Create: `tools/core/event-unsubscribe.toml`
- Create: `tools/core/event-list-subscriptions.toml`
- Create: `tools/core/event-emit.toml`
- Create: `tools/core/event-history.toml`

Use `tools/core/agent-message.toml` as the template pattern.

- [ ] **Step 1: Create `event-subscribe.toml`**

Create `tools/core/event-subscribe.toml`:

```toml
[manifest]
name        = "event-subscribe"
version     = "1.0.0"
description = "Subscribe an agent to OS or custom events with optional filters, throttling, and priority"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["event.self:w"]

[capabilities_provided]
outputs = ["status"]

[input_schema]
type = "object"
required = ["event_filter"]

[input_schema.properties.event_filter]
type = "string"
description = "Event type filter: 'all', 'category:AgentDefined', 'Custom:MyEvent', or an existing event type name"

[input_schema.properties.target_agent]
type = "string"
description = "Agent name to subscribe (omit for self; requires event.manage permission for other agents)"

[input_schema.properties.payload_filter]
type = "string"
description = "Optional payload predicate, e.g. 'severity == Critical AND cpu_percent > 85'"

[input_schema.properties.throttle]
type = "string"
description = "Throttle policy: 'none' (default), 'once_per:30s', 'max:5/60s'"

[input_schema.properties.priority]
type = "string"
description = "Subscription priority: 'critical', 'high', 'normal' (default), 'low'"
enum = ["critical", "high", "normal", "low"]
default = "normal"

[input_schema.properties.scope]
type = "string"
description = "Subscription lifetime: 'agent' (default, permanent) or 'task' (auto-removed when task ends)"
enum = ["agent", "task"]
default = "agent"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

- [ ] **Step 2: Create `event-unsubscribe.toml`**

Create `tools/core/event-unsubscribe.toml`:

```toml
[manifest]
name        = "event-unsubscribe"
version     = "1.0.0"
description = "Remove an event subscription by ID"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["event.self:w"]

[capabilities_provided]
outputs = ["status"]

[input_schema]
type = "object"
required = ["subscription_id"]

[input_schema.properties.subscription_id]
type = "string"
description = "UUID of the subscription to remove"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

- [ ] **Step 3: Create `event-list-subscriptions.toml`**

Create `tools/core/event-list-subscriptions.toml`:

```toml
[manifest]
name        = "event-list-subscriptions"
version     = "1.0.0"
description = "List event subscriptions for the current agent or a named agent"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["event.self:r"]

[capabilities_provided]
outputs = ["content.text"]

[input_schema]
type = "object"

[input_schema.properties.target_agent]
type = "string"
description = "Agent name to list subscriptions for (omit for self; requires event.manage permission for other agents)"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

- [ ] **Step 4: Create `event-emit.toml`**

Create `tools/core/event-emit.toml`:

```toml
[manifest]
name        = "event-emit"
version     = "1.0.0"
description = "Emit a custom event to the OS event bus for other agents to react to"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["event.self:w"]

[capabilities_provided]
outputs = ["status"]

[input_schema]
type = "object"
required = ["event_type"]

[input_schema.properties.event_type]
type = "string"
description = "Custom event type name (e.g. 'DataPipelineComplete', 'AnomalyDetected')"

[input_schema.properties.payload]
type = "object"
description = "Arbitrary JSON payload attached to the event"

[input_schema.properties.severity]
type = "string"
description = "Event severity level"
enum = ["info", "warning", "critical"]
default = "info"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

- [ ] **Step 5: Create `event-history.toml`**

Create `tools/core/event-history.toml`:

```toml
[manifest]
name        = "event-history"
version     = "1.0.0"
description = "View recent event history from the OS event bus"
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["event.self:r"]

[capabilities_provided]
outputs = ["content.text"]

[input_schema]
type = "object"

[input_schema.properties.last]
type = "integer"
description = "Number of recent events to return"
default = 20
minimum = 1
maximum = 200

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 16
max_cpu_ms    = 1000
syscalls      = []
```

- [ ] **Step 6: Build the full workspace to ensure manifests load**

Run: `cargo build --workspace 2>&1 | head -20`
Expected: Clean build. Manifest files are loaded at kernel startup, not compile time, so this just verifies no Rust breakage.

- [ ] **Step 7: Commit**

```bash
git add tools/core/event-subscribe.toml \
       tools/core/event-unsubscribe.toml \
       tools/core/event-list-subscriptions.toml \
       tools/core/event-emit.toml \
       tools/core/event-history.toml
git commit -m "feat(manifests): add tool manifests for 5 event management tools"
```

---

### Task 8: `KernelAction` Variants and `from_tool_result()` Parsing

**Files:**
- Modify: `crates/agentos-kernel/src/kernel_action.rs`

- [ ] **Step 1: Add 5 new variants to `KernelAction` enum**

Open `crates/agentos-kernel/src/kernel_action.rs`. The `KernelAction` enum ends at line 79. Add before the closing brace, after the `ContextMemoryRead` variant:

```rust
    /// Subscribe an agent to events.
    EventSubscribe {
        target_agent: Option<String>,
        event_filter: String,
        payload_filter: Option<String>,
        throttle: Option<String>,
        priority: Option<String>,
        scope: Option<String>,
    },
    /// Remove an event subscription.
    EventUnsubscribe {
        subscription_id: String,
    },
    /// List event subscriptions for an agent.
    EventListSubscriptions {
        target_agent: Option<String>,
    },
    /// Emit a custom event to the event bus.
    EventEmit {
        event_type: String,
        payload: serde_json::Value,
        severity: Option<String>,
    },
    /// Query recent event history.
    EventHistory {
        last: u32,
    },
```

- [ ] **Step 2: Add parsing arms to `from_tool_result()`**

In the `from_tool_result()` method (line 106), find the catch-all `other` arm at line 268. Add the five new arms before it:

```rust
            "event_subscribe" => {
                let event_filter = value.get("event_filter")?.as_str()?.to_string();
                let target_agent = value
                    .get("target_agent")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let payload_filter = value
                    .get("payload_filter")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let throttle = value
                    .get("throttle")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let priority = value
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let scope = value
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Some(Self::EventSubscribe {
                    target_agent,
                    event_filter,
                    payload_filter,
                    throttle,
                    priority,
                    scope,
                })
            }
            "event_unsubscribe" => {
                let subscription_id = value.get("subscription_id")?.as_str()?.to_string();
                Some(Self::EventUnsubscribe { subscription_id })
            }
            "event_list_subscriptions" => {
                let target_agent = value
                    .get("target_agent")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Some(Self::EventListSubscriptions { target_agent })
            }
            "event_emit" => {
                let event_type = value.get("event_type")?.as_str()?.to_string();
                let payload = value
                    .get("payload")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                let severity = value
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Some(Self::EventEmit {
                    event_type,
                    payload,
                    severity,
                })
            }
            "event_history" => {
                let last = value
                    .get("last")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20) as u32;
                Some(Self::EventHistory { last })
            }
```

- [ ] **Step 3: Add `action_name` entries in `dispatch_kernel_action()`**

In `dispatch_kernel_action()`, the `action_name` match at line 288-302 maps each variant to a string. Add after the `ContextMemoryRead` arm (line 301):

```rust
            KernelAction::EventSubscribe { .. } => "event_subscribe",
            KernelAction::EventUnsubscribe { .. } => "event_unsubscribe",
            KernelAction::EventListSubscriptions { .. } => "event_list_subscriptions",
            KernelAction::EventEmit { .. } => "event_emit",
            KernelAction::EventHistory { .. } => "event_history",
```

- [ ] **Step 4: Build to verify parsing compiles**

Run: `cargo build -p agentos-kernel 2>&1 | head -30`
Expected: Compile error — the `match action` block at line 317 is now non-exhaustive (missing the 5 new arms). That's expected and will be fixed in Task 9.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-kernel/src/kernel_action.rs
git commit -m "feat(kernel-action): add 5 event KernelAction variants with from_tool_result parsing"
```

---

### Task 9: `dispatch_kernel_action()` Handlers

**Files:**
- Modify: `crates/agentos-kernel/src/kernel_action.rs`

This is the core task — wiring the five handlers into the kernel's dispatch. Each handler follows the pattern of existing handlers (resolve state, check permissions, perform action, return result).

- [ ] **Step 1: Add the `EventSubscribe` handler**

In `dispatch_kernel_action()`, find the closing of the last match arm (`ContextMemoryRead`, ending around line 502). Add before `};` (the end of the `match action` block at line 503):

```rust
            KernelAction::EventSubscribe {
                target_agent,
                event_filter,
                payload_filter,
                throttle,
                priority,
                scope,
            } => {
                // 1. Resolve target agent
                let resolved_agent_id = if let Some(ref name) = target_agent {
                    // Managing another agent — check event.manage permission
                    if !task.capability_token.permissions.check("event.manage", PermissionOp::Write)
                    {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": "Permission denied: event.manage:w required to manage other agents' subscriptions",
                            }),
                        };
                    }
                    let registry = self.agent_registry.read().await;
                    match registry.find_by_name(name) {
                        Some(profile) => profile.id,
                        None => {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": format!("Agent '{}' not found", name),
                                }),
                            };
                        }
                    }
                } else {
                    task.agent_id
                };

                // 2. Parse event filter
                let type_filter = match crate::event_bus::parse_event_type_filter(&event_filter) {
                    Some(f) => f,
                    None => {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": format!("Invalid event filter: '{}'", event_filter),
                            }),
                        };
                    }
                };

                // 3. Parse optional throttle and priority
                let throttle_policy = if let Some(ref t) = throttle {
                    match crate::commands::event::parse_throttle(t) {
                        Some(tp) => tp,
                        None => {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": format!("Invalid throttle: '{}'", t),
                                }),
                            };
                        }
                    }
                } else {
                    agentos_types::ThrottlePolicy::None
                };

                let sub_priority = crate::event_bus::parse_subscription_priority(
                    priority.as_deref(),
                )
                .unwrap_or(agentos_types::SubscriptionPriority::Normal);

                // 4. Create and register subscription
                let sub_id = agentos_types::SubscriptionID::new();
                let subscription = agentos_types::EventSubscription {
                    id: sub_id,
                    agent_id: resolved_agent_id,
                    event_type_filter: type_filter,
                    filter: payload_filter,
                    priority: sub_priority,
                    throttle: throttle_policy,
                    enabled: true,
                    created_at: chrono::Utc::now(),
                };
                self.event_bus.subscribe(subscription).await;

                // 5. Task-scoped cleanup if requested
                let effective_scope = scope.as_deref().unwrap_or("agent");
                if effective_scope == "task" {
                    self.register_task_subscription(task.id, sub_id).await;
                }

                // 6. Audit
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::EventSubscriptionCreated,
                    agent_id: Some(resolved_agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "subscription_id": sub_id.to_string(),
                        "event_filter": event_filter,
                        "scope": effective_scope,
                        "source": "tool",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: Some(sub_id.to_string()),
                });

                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "subscription_id": sub_id.to_string(),
                        "event_filter": event_filter,
                        "scope": effective_scope,
                    }),
                }
            }
```

- [ ] **Step 2: Add the `EventUnsubscribe` handler**

```rust
            KernelAction::EventUnsubscribe { subscription_id } => {
                let sub_id = match subscription_id.parse::<uuid::Uuid>() {
                    Ok(uuid) => agentos_types::SubscriptionID::from(uuid),
                    Err(_) => {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": format!("Invalid subscription ID: '{}'", subscription_id),
                            }),
                        };
                    }
                };

                // Check ownership
                let sub = self.event_bus.get_subscription(&sub_id).await;
                if let Some(ref s) = sub {
                    if s.agent_id != task.agent_id
                        && !task
                            .capability_token
                            .permissions
                            .check("event.manage", PermissionOp::Write)
                    {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": "Permission denied: cannot unsubscribe another agent without event.manage:w",
                            }),
                        };
                    }
                }

                let removed = self.event_bus.unsubscribe(&sub_id).await;

                // Clean up from task-scoped map
                self.remove_task_subscription(&task.id, &sub_id).await;

                if removed {
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id,
                        event_type: agentos_audit::AuditEventType::EventSubscriptionRemoved,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "subscription_id": subscription_id,
                            "source": "tool",
                        }),
                        severity: agentos_audit::AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    });
                }

                KernelActionResult {
                    success: true,
                    result: serde_json::json!({ "removed": removed }),
                }
            }
```

- [ ] **Step 3: Add the `EventListSubscriptions` handler**

```rust
            KernelAction::EventListSubscriptions { target_agent } => {
                let resolved_agent_id = if let Some(ref name) = target_agent {
                    if !task
                        .capability_token
                        .permissions
                        .check("event.manage", PermissionOp::Read)
                    {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": "Permission denied: event.manage:r required to list other agents' subscriptions",
                            }),
                        };
                    }
                    let registry = self.agent_registry.read().await;
                    match registry.find_by_name(name) {
                        Some(profile) => profile.id,
                        None => {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": format!("Agent '{}' not found", name),
                                }),
                            };
                        }
                    }
                } else {
                    task.agent_id
                };

                let subs = self
                    .event_bus
                    .list_subscriptions_for_agent(&resolved_agent_id)
                    .await;
                let entries: Vec<serde_json::Value> = subs
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id.to_string(),
                            "event_type_filter": format!("{:?}", s.event_type_filter),
                            "priority": format!("{:?}", s.priority),
                            "throttle": format!("{:?}", s.throttle),
                            "enabled": s.enabled,
                            "created_at": s.created_at.to_rfc3339(),
                        })
                    })
                    .collect();

                KernelActionResult {
                    success: true,
                    result: serde_json::json!({ "subscriptions": entries }),
                }
            }
```

- [ ] **Step 4: Add the `EventEmit` handler**

```rust
            KernelAction::EventEmit {
                event_type,
                payload,
                severity,
            } => {
                // Rate limit check
                if !self.check_agent_event_rate(&task.agent_id).await {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": "Rate limit exceeded: too many events emitted per second. Retry after 1 second.",
                        }),
                    };
                }

                // Parse severity
                let sev = match severity.as_deref() {
                    Some("warning") => agentos_types::EventSeverity::Warning,
                    Some("critical") => agentos_types::EventSeverity::Critical,
                    _ => agentos_types::EventSeverity::Info,
                };

                // Determine chain depth from trigger source
                let chain_depth = task
                    .trigger_source
                    .as_ref()
                    .map(|ts| ts.chain_depth + 1)
                    .unwrap_or(0);

                // Emit as Custom event with Agent source
                let event_id = agentos_types::EventID::new();
                crate::event_dispatch::emit_signed_event(
                    &self.capability_engine,
                    &self.audit,
                    &self.event_sender,
                    agentos_types::EventType::Custom(event_type.clone()),
                    agentos_types::EventSource::Agent(task.agent_id),
                    sev,
                    payload,
                    chain_depth,
                    trace_id,
                    Some(task.agent_id),
                    Some(task.id),
                );

                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "event_id": event_id.to_string(),
                        "event_type": event_type,
                        "delivered": true,
                    }),
                }
            }
```

Note: Check the exact signature of `emit_signed_event()` — it may return the `EventID` or take it as a parameter. Adapt to match the actual function signature. If `emit_signed_event` generates its own ID internally, remove the `event_id` local and use a placeholder or retrieve it from the function's return.

- [ ] **Step 5: Add the `EventHistory` handler**

```rust
            KernelAction::EventHistory { last } => {
                // Reuse the same audit query as cmd_event_history
                let entries = self
                    .audit
                    .query_by_type(
                        agentos_audit::AuditEventType::EventEmitted,
                        last as usize,
                    )
                    .unwrap_or_default();

                let events: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "timestamp": e.timestamp.to_rfc3339(),
                            "details": e.details,
                            "severity": format!("{:?}", e.severity),
                        })
                    })
                    .collect();

                KernelActionResult {
                    success: true,
                    result: serde_json::json!({ "events": events }),
                }
            }
```

Note: Check if `self.audit.query_by_type()` exists. The existing `cmd_event_history` handler in `commands/event.rs` uses a specific audit query method — follow the same pattern. You may need to use `self.audit.recent_by_type()` or similar. Read the actual audit API in `crates/agentos-audit/src/lib.rs` and adapt.

- [ ] **Step 6: Add the `task_executor.rs` kernel action detection**

In `crates/agentos-kernel/src/task_executor.rs`, find where `_kernel_action` results are matched for the `memory_mutating_action` flag (around line 1370-1380). Add the new action names to the non-memory-mutating set. The existing code has a pattern like:

```rust
matches!(
    action,
    KernelAction::MemoryBlockWrite { .. }
    | ...
)
```

The event actions are NOT memory-mutating, so they should NOT be in this list. But verify the match arms compile — the exhaustive match in `dispatch_kernel_action` should now cover all variants.

- [ ] **Step 7: Build**

Run: `cargo build --workspace 2>&1 | head -40`
Expected: Clean build. All exhaustive matches should now be complete.

- [ ] **Step 8: Run all tests**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: All pass.

- [ ] **Step 9: Commit**

```bash
git add crates/agentos-kernel/src/kernel_action.rs crates/agentos-kernel/src/task_executor.rs
git commit -m "feat(kernel): implement dispatch handlers for 5 event KernelAction variants"
```

---

### Task 10: Integration Tests

**Files:**
- Modify: Tests in `crates/agentos-kernel/` or `crates/agentos-tools/`

- [ ] **Step 1: Test `from_tool_result()` parsing for all 5 actions**

Add to the test module in `kernel_action.rs` (or create one if it doesn't exist):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_event_subscribe_action() {
        let result = serde_json::json!({
            "_kernel_action": "event_subscribe",
            "event_filter": "Custom:DataReady",
            "target_agent": "analyzer",
            "priority": "high",
            "scope": "task",
        });
        let action = KernelAction::from_tool_result(&result).unwrap();
        match action {
            KernelAction::EventSubscribe {
                target_agent,
                event_filter,
                priority,
                scope,
                ..
            } => {
                assert_eq!(event_filter, "Custom:DataReady");
                assert_eq!(target_agent.as_deref(), Some("analyzer"));
                assert_eq!(priority.as_deref(), Some("high"));
                assert_eq!(scope.as_deref(), Some("task"));
            }
            other => panic!("Expected EventSubscribe, got {:?}", other),
        }
    }

    #[test]
    fn parse_event_unsubscribe_action() {
        let result = serde_json::json!({
            "_kernel_action": "event_unsubscribe",
            "subscription_id": "550e8400-e29b-41d4-a716-446655440000",
        });
        let action = KernelAction::from_tool_result(&result).unwrap();
        match action {
            KernelAction::EventUnsubscribe { subscription_id } => {
                assert_eq!(subscription_id, "550e8400-e29b-41d4-a716-446655440000");
            }
            other => panic!("Expected EventUnsubscribe, got {:?}", other),
        }
    }

    #[test]
    fn parse_event_list_subscriptions_action() {
        let result = serde_json::json!({
            "_kernel_action": "event_list_subscriptions",
            "target_agent": null,
        });
        let action = KernelAction::from_tool_result(&result).unwrap();
        match action {
            KernelAction::EventListSubscriptions { target_agent } => {
                assert!(target_agent.is_none());
            }
            other => panic!("Expected EventListSubscriptions, got {:?}", other),
        }
    }

    #[test]
    fn parse_event_emit_action() {
        let result = serde_json::json!({
            "_kernel_action": "event_emit",
            "event_type": "DataPipelineComplete",
            "payload": { "dataset": "sales-q1" },
            "severity": "warning",
        });
        let action = KernelAction::from_tool_result(&result).unwrap();
        match action {
            KernelAction::EventEmit {
                event_type,
                payload,
                severity,
            } => {
                assert_eq!(event_type, "DataPipelineComplete");
                assert_eq!(payload["dataset"], "sales-q1");
                assert_eq!(severity.as_deref(), Some("warning"));
            }
            other => panic!("Expected EventEmit, got {:?}", other),
        }
    }

    #[test]
    fn parse_event_history_action() {
        let result = serde_json::json!({
            "_kernel_action": "event_history",
            "last": 50,
        });
        let action = KernelAction::from_tool_result(&result).unwrap();
        match action {
            KernelAction::EventHistory { last } => {
                assert_eq!(last, 50);
            }
            other => panic!("Expected EventHistory, got {:?}", other),
        }
    }

    #[test]
    fn parse_event_emit_defaults() {
        let result = serde_json::json!({
            "_kernel_action": "event_emit",
            "event_type": "Heartbeat",
        });
        let action = KernelAction::from_tool_result(&result).unwrap();
        match action {
            KernelAction::EventEmit {
                event_type,
                payload,
                severity,
            } => {
                assert_eq!(event_type, "Heartbeat");
                assert_eq!(payload, serde_json::json!({}));
                assert!(severity.is_none());
            }
            other => panic!("Expected EventEmit, got {:?}", other),
        }
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p agentos-kernel -- kernel_action::tests -- --nocapture 2>&1 | tail -20`
Expected: All 6 tests pass.

- [ ] **Step 3: Test tool execute methods return correct JSON**

Add tests to each tool file, or create a combined test. Example for `event_emit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::ToolExecutionContext;
    use std::path::PathBuf;

    fn test_context() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: PathBuf::from("/tmp/test"),
            task_id: agentos_types::TaskID::new(),
            agent_id: agentos_types::AgentID::new(),
            trace_id: agentos_types::TraceID::new(),
            permissions: agentos_types::PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn emit_returns_kernel_action() {
        let tool = EventEmitTool::new();
        let payload = serde_json::json!({
            "event_type": "TestEvent",
            "payload": { "key": "value" },
            "severity": "warning",
        });
        let result = tool.execute(payload, test_context()).await.unwrap();
        assert_eq!(result["_kernel_action"], "event_emit");
        assert_eq!(result["event_type"], "TestEvent");
        assert_eq!(result["payload"]["key"], "value");
        assert_eq!(result["severity"], "warning");
    }

    #[tokio::test]
    async fn emit_requires_event_type() {
        let tool = EventEmitTool::new();
        let payload = serde_json::json!({ "payload": {} });
        let result = tool.execute(payload, test_context()).await;
        assert!(result.is_err());
    }
}
```

Follow the same pattern for the other 4 tools. Each test should verify:
1. Correct `_kernel_action` value
2. All fields properly passed through
3. Required fields validated (returns `Err` when missing)

- [ ] **Step 4: Run all tool tests**

Run: `cargo test -p agentos-tools 2>&1 | tail -20`
Expected: All pass.

- [ ] **Step 5: Run full workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: All pass.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -20`
Expected: No new warnings (pre-existing ones in agentos-audit/sandbox/memory are OK).

- [ ] **Step 7: Run fmt check**

Run: `cargo fmt --all -- --check 2>&1 | tail -10`
Expected: No formatting issues.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "test: add unit tests for event tool parsing and execution"
```

---

### Task 11: Final Verification

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace 2>&1 | tail -5`
Expected: Clean build, no errors.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: All pass.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -20`
Expected: No new warnings.

- [ ] **Step 4: Format check**

Run: `cargo fmt --all -- --check`
Expected: Clean.

- [ ] **Step 5: Verify manifests are valid TOML**

Run: `for f in tools/core/event-*.toml; do echo "--- $f ---"; python3 -c "import tomllib; tomllib.load(open('$f','rb')); print('OK')"; done`
Expected: All 5 print "OK".

- [ ] **Step 6: Verify all new files exist**

Run: `ls -la crates/agentos-tools/src/event_*.rs tools/core/event-*.toml`
Expected: 5 Rust files + 5 TOML files = 10 new files.
