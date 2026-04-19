---
title: "Phase 2: Native Tool Result Formatting"
tags:
  - llm
  - v3
  - plan
date: 2026-03-24
status: complete
effort: 2d
priority: critical
---

# Phase 2: Native Tool Result Formatting

> Make each adapter format tool results in its provider's native protocol, and add `ContextRole::Tool` so the kernel can inject tool results correctly for multi-turn tool use loops.

---

## Why This Phase

The current adapters map `ContextRole::ToolResult` to `role: "user"` with a "Tool Result:" prefix. This is incorrect for every provider:

- **OpenAI** requires `role: "tool"` messages with a `tool_call_id` matching the assistant's tool call.
- **Anthropic** requires `role: "user"` messages with a `tool_result` content block containing `tool_use_id`.
- **Gemini** requires `role: "user"` messages with a `functionResponse` part containing the function name and result.
- **Ollama** requires `role: "tool"` messages.

Without correct formatting, multi-turn tool loops either fail (OpenAI rejects the request), produce degraded results (the model doesn't understand the result is from a tool), or waste tokens on the text prefix.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `ContextRole` enum | `System, User, Assistant, ToolResult` | Add `Tool` variant (details below) |
| `ContextEntry` for tool results | Uses `ContextRole::ToolResult`, plain text content | Uses `ContextRole::Tool`, has `tool_call_id` and `tool_name` in metadata |
| `ContextMetadata` | Unstructured `Option<ContextMetadata>` | Add `tool_call_id: Option<String>`, `tool_name: Option<String>` fields |
| OpenAI `format_messages` | ToolResult -> `"user"` | Tool -> `"tool"` with `tool_call_id` field |
| Anthropic `format_messages` | ToolResult -> `"user"` text | Tool -> `"user"` with `[{"type": "tool_result", ...}]` content blocks |
| Gemini `format_contents` | ToolResult -> `"user"` text | Tool -> `"user"` with `[{"functionResponse": {...}}]` parts |
| Ollama `context_to_messages` | ToolResult -> `"user"` | Tool -> `"tool"` with content |

---

## What to Do

### Step 1: Extend `ContextMetadata` in `agentos-types`

Open `crates/agentos-types/src/context.rs`. The `ContextMetadata` struct needs `tool_call_id` and `tool_name` fields. Find the current definition and add:

```rust
/// Metadata attached to a context entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMetadata {
    // ... existing fields ...
    /// Provider-native tool call ID for tool result entries.
    /// Used by adapters to format tool results in the correct provider protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool name for tool result entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}
```

If `ContextMetadata` is currently a simple type or has no optional fields, add them with `#[serde(default)]` for backward compatibility.

### Step 2: Keep `ContextRole::ToolResult` for backward compatibility

Do NOT remove `ContextRole::ToolResult`. Instead, the adapters will check for `ContextRole::ToolResult` entries that have `metadata.tool_call_id` set and format them natively. Entries without `tool_call_id` use the legacy "Tool Result:" prefix for backward compatibility.

### Step 3: Update OpenAI `format_messages`

Open `crates/agentos-llm/src/openai.rs`. Change `format_messages`:

```rust
fn format_messages(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();

    for entry in context.active_entries() {
        match entry.role {
            ContextRole::ToolResult => {
                // Check for native tool result metadata.
                let tool_call_id = entry.metadata.as_ref()
                    .and_then(|m| m.tool_call_id.as_deref());

                if let Some(call_id) = tool_call_id {
                    // Native OpenAI tool result format.
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": entry.content,
                    }));
                } else {
                    // Legacy fallback: plain user message.
                    messages.push(json!({
                        "role": "user",
                        "content": format!("Tool Result:\n{}", entry.content),
                    }));
                }
            }
            ContextRole::System => {
                messages.push(json!({
                    "role": "system",
                    "content": entry.content,
                }));
            }
            ContextRole::User => {
                messages.push(json!({
                    "role": "user",
                    "content": entry.content,
                }));
            }
            ContextRole::Assistant => {
                messages.push(json!({
                    "role": "assistant",
                    "content": entry.content,
                }));
            }
        }
    }

    messages
}
```

### Step 4: Update Anthropic `format_messages`

Open `crates/agentos-llm/src/anthropic.rs`. Anthropic requires tool results as user messages with `tool_result` content blocks. Consecutive tool results must be batched into a single user message:

```rust
fn format_messages(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();
    let mut pending_tool_results: Vec<serde_json::Value> = Vec::new();

    let flush_tool_results = |messages: &mut Vec<Value>, pending: &mut Vec<Value>| {
        if !pending.is_empty() {
            messages.push(json!({
                "role": "user",
                "content": std::mem::take(pending),
            }));
        }
    };

    for entry in context.active_entries() {
        match entry.role {
            ContextRole::System => continue,
            ContextRole::ToolResult => {
                let tool_use_id = entry.metadata.as_ref()
                    .and_then(|m| m.tool_call_id.as_deref());

                if let Some(use_id) = tool_use_id {
                    pending_tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": use_id,
                        "content": entry.content,
                    }));
                } else {
                    flush_tool_results(&mut messages, &mut pending_tool_results);
                    messages.push(json!({
                        "role": "user",
                        "content": format!("Tool Result:\n{}", entry.content),
                    }));
                }
            }
            _ => {
                flush_tool_results(&mut messages, &mut pending_tool_results);
                let role = match entry.role {
                    ContextRole::User => "user",
                    ContextRole::Assistant => "assistant",
                    _ => unreachable!(),
                };
                messages.push(json!({
                    "role": role,
                    "content": entry.content,
                }));
            }
        }
    }
    flush_tool_results(&mut messages, &mut pending_tool_results);

    messages
}
```

### Step 5: Update Gemini `format_contents`

Open `crates/agentos-llm/src/gemini.rs`. Gemini uses `functionResponse` parts:

```rust
fn format_contents(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
    let mut contents = Vec::new();

    for entry in context.active_entries() {
        match entry.role {
            ContextRole::System => continue,
            ContextRole::ToolResult => {
                let tool_name = entry.metadata.as_ref()
                    .and_then(|m| m.tool_name.as_deref());

                if let Some(name) = tool_name {
                    // Parse content as JSON for structured response, fallback to string.
                    let response_val = serde_json::from_str::<Value>(&entry.content)
                        .unwrap_or_else(|_| json!({"result": entry.content}));
                    contents.push(json!({
                        "role": "user",
                        "parts": [{
                            "functionResponse": {
                                "name": name,
                                "response": response_val,
                            }
                        }]
                    }));
                } else {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{"text": format!("Tool Result:\n{}", entry.content)}]
                    }));
                }
            }
            _ => {
                let role = match entry.role {
                    ContextRole::User => "user",
                    ContextRole::Assistant => "model",
                    _ => continue,
                };
                contents.push(json!({
                    "role": role,
                    "parts": [{"text": entry.content.clone()}]
                }));
            }
        }
    }

    contents
}
```

### Step 6: Update Ollama `context_to_messages`

Open `crates/agentos-llm/src/ollama.rs`. Ollama uses `role: "tool"`:

```rust
fn context_to_messages(&self, context: &ContextWindow) -> Vec<OllamaChatMessage> {
    context.active_entries().iter().map(|entry| {
        let role = match entry.role {
            ContextRole::System => "system",
            ContextRole::User => "user",
            ContextRole::Assistant => "assistant",
            ContextRole::ToolResult => {
                if entry.metadata.as_ref().and_then(|m| m.tool_call_id.as_deref()).is_some() {
                    "tool"
                } else {
                    "user"
                }
            }
        };
        OllamaChatMessage {
            role: role.to_string(),
            content: entry.content.clone(),
            tool_calls: Vec::new(),
        }
    }).collect()
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/context.rs` | Add `tool_call_id` and `tool_name` fields to `ContextMetadata` |
| `crates/agentos-llm/src/openai.rs` | Update `format_messages` for native `role: "tool"` with `tool_call_id` |
| `crates/agentos-llm/src/anthropic.rs` | Update `format_messages` for native `tool_result` content blocks |
| `crates/agentos-llm/src/gemini.rs` | Update `format_contents` for native `functionResponse` parts |
| `crates/agentos-llm/src/ollama.rs` | Update `context_to_messages` for `role: "tool"` |
| `crates/agentos-llm/src/custom.rs` | No change (custom uses OpenAI-compatible format, no native tool protocol) |

---

## Prerequisites

[[01-core-types-and-trait-redesign]] must be complete (for extended `InferenceResult` struct fields).

---

## Test Plan

- `cargo build --workspace` must pass
- `cargo test -p agentos-llm` -- all existing tests pass
- Add test in `openai.rs`: `test_format_messages_native_tool_result` -- context with `ContextRole::ToolResult` + `tool_call_id` metadata produces `role: "tool"` message
- Add test in `openai.rs`: `test_format_messages_legacy_tool_result` -- context with `ContextRole::ToolResult` WITHOUT metadata produces `role: "user"` with "Tool Result:" prefix (backward compat)
- Add test in `anthropic.rs`: `test_format_messages_native_tool_result` -- produces `tool_result` content blocks
- Add test in `gemini.rs`: `test_format_contents_native_tool_result` -- produces `functionResponse` parts
- Add test in `ollama.rs`: `test_context_to_messages_native_tool_result` -- produces `role: "tool"`

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-llm -- --nocapture
cargo test -p agentos-types -- --nocapture
cargo clippy -p agentos-llm -- -D warnings
cargo fmt --all -- --check
```
