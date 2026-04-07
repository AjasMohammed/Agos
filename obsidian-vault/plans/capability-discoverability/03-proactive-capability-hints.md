---
title: "Phase 3: Proactive Capability Hints"
tags:
  - kernel
  - agents
  - v4
  - plan
date: 2026-04-07
status: planned
effort: 0.5d
priority: medium
---

# Phase 3: Proactive Capability Hints

> Inject lightweight tool suggestions into the context window when the agent's last message indicates an intent that matches an existing tool.

---

## Why This Phase

Pull-based discovery (Phase 1's `suggest` section) only works if the agent thinks to ask. Proactive hints push relevant tool suggestions to the agent at the right moment — when the LLM's output indicates it's about to reinvent something that already exists.

This is the equivalent of an IDE's autocomplete: the agent says "I need to store these intermediate results" and the system nudges "Available: scratch-write (scratchpad), memory-block-write (key-value)."

---

## Current → Target State

**Current:** The `ContextInjector` (`crates/agentos-kernel/src/context_injector.rs`) injects system context (memory, events, scratchpad notes) into the context window before each inference. No tool suggestion injection.

**Target:** The `ContextInjector` optionally scans the last assistant message, runs a semantic search against the tool embedding index, and injects a one-line hint if a high-confidence match is found.

---

## Detailed Subtasks

### 1. Add tool suggestion method to ContextInjector

**File:** `crates/agentos-kernel/src/context_injector.rs`

```rust
/// If the last assistant message suggests an intent that matches an available tool,
/// return a one-line hint. Returns None if no high-confidence match found.
fn suggest_tool_hint(
    &self,
    last_assistant_message: &str,
    tool_embeddings: &[(String, Vec<f32>, String)], // (name, embedding, description)
    embedder: &Embedder,
    threshold: f32,
) -> Option<String> {
    // Only scan messages that look like "I need to..." or "I'll..." or "Let me..."
    // to avoid suggesting tools when the agent is already using one
    let intent_patterns = ["I need to", "I'll ", "Let me ", "I should ", "I want to",
                           "I'm going to", "We need to", "Next I'll"];
    let has_intent = intent_patterns.iter().any(|p| last_assistant_message.contains(p));
    if !has_intent { return None; }

    let query_emb = embedder.embed(last_assistant_message).ok()?;

    let mut best_score = 0.0f32;
    let mut best_tool = "";
    let mut best_desc = "";

    for (name, emb, desc) in tool_embeddings {
        let score = cosine_similarity(&query_emb, emb);
        if score > best_score {
            best_score = score;
            best_tool = name;
            best_desc = desc;
        }
    }

    if best_score >= threshold {
        Some(format!("💡 Available tool: `{}` — {}", best_tool, best_desc))
    } else {
        None
    }
}
```

### 2. Integrate into injection pipeline

**File:** `crates/agentos-kernel/src/context_injector.rs`

In the main `inject()` method, after existing injections:

```rust
if self.config.proactive_discovery {
    if let Some(last_msg) = context.last_assistant_message() {
        if let Some(hint) = self.suggest_tool_hint(
            last_msg, &self.tool_embeddings, &self.embedder, 0.7
        ) {
            context.inject_system_note(&hint);
        }
    }
}
```

### 3. Add kernel config option

**File:** `config/default.toml`

```toml
[tools]
# When true, the context injector suggests relevant tools when the agent
# expresses an intent that matches an available tool. Default: false.
proactive_discovery = false
```

**File:** `crates/agentos-kernel/src/kernel.rs` — read `proactive_discovery` from config and pass to `ContextInjector`.

### 4. Pass tool embeddings to ContextInjector

**File:** `crates/agentos-kernel/src/kernel.rs`

After the tool registry is loaded and `AgentManualTool` is constructed with embeddings, share the same embedding data with the `ContextInjector`. This avoids recomputing embeddings.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/context_injector.rs` | Add `suggest_tool_hint()`, integrate into `inject()` |
| `crates/agentos-kernel/src/kernel.rs` | Pass tool embeddings and config to `ContextInjector` |
| `config/default.toml` | Add `proactive_discovery` option |

---

## Dependencies

- **Requires:** Phase 1 (embedding index), Phase 2 (capability tags for richer matching)
- **Blocks:** Nothing — this is the final phase

---

## Test Plan

1. **Hint triggered** — set `proactive_discovery = true`, inject context with last assistant message "I need to save these intermediate results for later use"; verify hint contains `scratch-write`
2. **Hint suppressed below threshold** — message "Hello, how are you?"; verify no hint injected
3. **Hint suppressed when disabled** — set `proactive_discovery = false`; verify no hint even with matching message
4. **Max one hint per iteration** — verify only one hint is injected even if multiple tools match
5. **No hint when agent is already using a tool** — message contains a tool call result; verify no hint

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- context_injector
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
