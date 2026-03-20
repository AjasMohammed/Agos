---
title: Agent Manual Integration Tests
tags:
  - tools
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 3h
priority: high
---

# Agent Manual Integration Tests

> Add comprehensive integration tests that exercise every manual section through the `ToolRunner::execute()` path, verifying permission-free access, correct JSON structure, and error handling for invalid queries.

---

## Why This Subtask

Unit tests in subtask 02 verify each section method in isolation. Integration tests verify the full pipeline: JSON payload -> `AgentTool::execute()` -> `ToolRunner::execute()` -> response. This catches issues like: missing section dispatch, permission check failures on a no-permission tool, serialization bugs, or incorrect error types for invalid input.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Integration tests for `agent-manual` | None | 12+ test cases covering all sections, error paths, and edge cases |

---

## What to Do

### 1. Add integration tests in `crates/agentos-tools/src/lib.rs`

Add these tests inside the existing `#[cfg(test)] mod tests` block in `crates/agentos-tools/src/lib.rs`. They use the existing `make_context()` helper.

```rust
// ── agent-manual integration tests ───────────────────────────────

#[tokio::test]
async fn test_agent_manual_index_section() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "index"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["section"], "index");
    assert!(result["sections"].as_array().unwrap().len() >= 8);
    assert!(result["usage"].as_str().is_some());
}

#[tokio::test]
async fn test_agent_manual_tools_section_empty() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "tools"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["section"], "tools");
    assert_eq!(result["count"], 0);
    assert_eq!(result["tools"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_agent_manual_tools_section_with_tools() {
    let dir = TempDir::new().unwrap();
    let summaries = vec![
        crate::agent_manual::ToolSummary {
            name: "file-reader".into(),
            description: "Read files".into(),
            version: "1.1.0".into(),
            permissions: vec!["fs.user_data:r".into()],
            input_schema: None,
            trust_tier: "core".into(),
        },
        crate::agent_manual::ToolSummary {
            name: "http-client".into(),
            description: "HTTP requests".into(),
            version: "1.0.0".into(),
            permissions: vec!["network.outbound:x".into()],
            input_schema: None,
            trust_tier: "core".into(),
        },
    ];
    let tool = crate::agent_manual::AgentManualTool::new(summaries);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "tools"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["count"], 2);
    let tools = result["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"file-reader"));
    assert!(names.contains(&"http-client"));
}

#[tokio::test]
async fn test_agent_manual_tool_detail_found() {
    let dir = TempDir::new().unwrap();
    let summaries = vec![crate::agent_manual::ToolSummary {
        name: "file-reader".into(),
        description: "Read files from data directory".into(),
        version: "1.1.0".into(),
        permissions: vec!["fs.user_data:r".into()],
        input_schema: Some(serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}})),
        trust_tier: "core".into(),
    }];
    let tool = crate::agent_manual::AgentManualTool::new(summaries);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(
            serde_json::json!({"section": "tool-detail", "name": "file-reader"}),
            ctx,
        )
        .await
        .unwrap();
    assert_eq!(result["section"], "tool-detail");
    assert_eq!(result["name"], "file-reader");
    assert_eq!(result["version"], "1.1.0");
    assert!(result["input_schema"].is_object());
}

#[tokio::test]
async fn test_agent_manual_tool_detail_not_found() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(
            serde_json::json!({"section": "tool-detail", "name": "nonexistent"}),
            ctx,
        )
        .await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        AgentOSError::ToolNotFound(_)
    ));
}

#[tokio::test]
async fn test_agent_manual_tool_detail_missing_name() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "tool-detail"}), ctx)
        .await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        AgentOSError::SchemaValidation(_)
    ));
}

#[tokio::test]
async fn test_agent_manual_permissions_section() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "permissions"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["section"], "permissions");
    assert!(result["resource_classes"].as_array().unwrap().len() >= 5);
}

#[tokio::test]
async fn test_agent_manual_memory_section() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "memory"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["section"], "memory");
    let tiers = result["tiers"].as_array().unwrap();
    assert_eq!(tiers.len(), 3);
    let tier_names: Vec<&str> = tiers.iter().map(|t| t["tier"].as_str().unwrap()).collect();
    assert!(tier_names.contains(&"semantic"));
    assert!(tier_names.contains(&"episodic"));
    assert!(tier_names.contains(&"procedural"));
}

#[tokio::test]
async fn test_agent_manual_events_section() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "events"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["section"], "events");
    assert_eq!(result["categories"].as_array().unwrap().len(), 10);
}

#[tokio::test]
async fn test_agent_manual_commands_section() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "commands"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["section"], "commands");
    assert!(result["domains"].as_array().unwrap().len() >= 8);
}

#[tokio::test]
async fn test_agent_manual_errors_section() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "errors"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["section"], "errors");
    let errors = result["errors"].as_array().unwrap();
    assert!(errors.len() >= 5);
    // Verify each error has required fields
    for err in errors {
        assert!(err["error"].as_str().is_some());
        assert!(err["cause"].as_str().is_some());
        assert!(err["recovery"].as_str().is_some());
    }
}

#[tokio::test]
async fn test_agent_manual_feedback_section() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "feedback"}), ctx)
        .await
        .unwrap();
    assert_eq!(result["section"], "feedback");
    assert!(result["format"]["fields"].as_array().unwrap().len() >= 4);
    assert!(result["example"].as_str().unwrap().contains("[FEEDBACK]"));
}

#[tokio::test]
async fn test_agent_manual_invalid_section() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"section": "nonexistent"}), ctx)
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    assert!(err.to_string().contains("nonexistent"));
}

#[tokio::test]
async fn test_agent_manual_missing_section_field() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    let ctx = make_context(dir.path());
    let result = tool
        .execute(serde_json::json!({"query": "hello"}), ctx)
        .await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        AgentOSError::SchemaValidation(_)
    ));
}

#[tokio::test]
async fn test_agent_manual_requires_no_permissions() {
    let dir = TempDir::new().unwrap();
    let tool = crate::agent_manual::AgentManualTool::new(vec![]);
    // Use an empty permission set — should still work
    let ctx = make_context_with_permissions(dir.path(), PermissionSet::new());
    let result = tool
        .execute(serde_json::json!({"section": "index"}), ctx)
        .await;
    assert!(result.is_ok(), "agent-manual should work without any permissions");
}
```

### 2. Verify the tool name matches

Ensure the test uses `"agent-manual"` which matches `AgentManualTool::name()`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/lib.rs` | Add 14 integration tests in the existing `#[cfg(test)] mod tests` block |

---

## Prerequisites

[[27-03-Wire AgentManual into ToolRunner and Registry]] must be complete (the tool must be registered so runner-level tests work).

For the tests in this subtask, most test `AgentManualTool` directly (not through `ToolRunner`), so they can also work after just subtask 02. However, the runner-level test from subtask 03 must also pass.

---

## Test Plan

- All 14 new tests must pass.
- No existing tests should break.
- `cargo test -p agentos-tools` must pass in full.
- `cargo clippy -p agentos-tools -- -D warnings` must pass.

---

## Verification

```bash
cargo test -p agentos-tools -- agent_manual --nocapture
cargo test -p agentos-tools -- test_agent_manual --nocapture
cargo clippy -p agentos-tools -- -D warnings
cargo fmt --all -- --check
```
