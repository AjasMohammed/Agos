---
title: Phase 6 — Profile-Based Sanitizer
tags:
  - kernel
  - chat
  - llm
  - security
  - phase
date: 2026-04-12
status: in-progress
effort: 0.5d
priority: low
---

# Phase 6 — Profile-Based Sanitizer

> Consolidate all sanitization passes into a single `sanitize_visible_text` function with named profiles (delivery / history / debug) that control which passes run. Mirrors OpenClaw's `sanitizeAssistantVisibleTextWithProfile`.

---

## Why this phase

Today the kernel's `sanitize_chat_inference_result` runs a hardcoded sequence of passes (fenced extraction → XML stripping → optional `<final>` → error rewriting). That sequence is correct for **delivery** (the text the user sees live), but other consumers need different combinations:

| Consumer | What it needs | Which passes |
|---|---|---|
| **Delivery** (SSE stream, chat store, `ChatInferenceResult::answer`) | Maximally clean: hide all tool scaffolding, enforce `<final>`, rewrite errors | All passes |
| **History** (context window entry for the next LLM turn) | Clean enough to avoid re-tempting the leak format, but preserve reasoning prose | Fenced extraction + XML stripping only (no `<final>` filter, no error rewrite) |
| **Debug** (kernel tracing, developer inspection) | Raw text with minimal changes | None (or just fenced extraction) |

Phase 6 codifies these profiles as an enum, packages the pass pipeline into a single public function, and updates the kernel call sites. This simplifies the kernel code and creates a clean extension point for future passes.

## Detailed subtasks

### 1. Add `SanitizeProfile` and `sanitize_visible_text` to output_sanitizer.rs

```rust
/// Controls which output sanitization passes are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SanitizeProfile {
    /// Maximum filtering. Used for the user-facing SSE stream, persisted
    /// chat messages, and the final `ChatInferenceResult::answer`. Runs
    /// all passes: fenced-block extraction, XML stripping, optional
    /// `<final>` enforcement, and error-payload rewriting.
    Delivery,
    /// Used for the assistant context-window entry that feeds the next
    /// LLM turn. Strips fenced tool blocks and XML tool tags (to avoid
    /// re-tempting the model), but preserves reasoning prose, does not
    /// enforce `<final>`, and does not rewrite errors (the model should
    /// see errors so it can reason about retrying).
    History,
    /// Minimal filtering. Used for kernel tracing at debug level and
    /// developer inspection. Only strips fenced tool blocks (so they
    /// don't clutter logs) but preserves everything else.
    Debug,
}

/// Result of running the sanitization pipeline.
pub struct SanitizeResult {
    /// The cleaned text for the given profile.
    pub text: String,
    /// Tool intents extracted from fenced blocks (present for all profiles).
    pub extracted_intents: Vec<ExtractedToolIntent>,
}

/// Run the output sanitization pipeline for `profile` on `text`.
pub fn sanitize_visible_text(
    text: &str,
    profile: SanitizeProfile,
    enforce_final_tag: bool,
) -> SanitizeResult;
```

### 2. Refactor `sanitize_chat_inference_result` in kernel.rs

Replace the inline pass sequence with two calls:

```rust
// For the context window entry (next LLM turn):
let history = sanitize_visible_text(&result.text, SanitizeProfile::History, false);
result.text = history.text;
// Promote extracted intents from the history pass (same logic as before).

// For the user-facing answer:
let delivery = sanitize_visible_text(&result.text, SanitizeProfile::Delivery, enforce_final_tag);
let visible_text = delivery.text;
```

### 3. Files changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/output_sanitizer.rs` | Add `SanitizeProfile`, `SanitizeResult`, `sanitize_visible_text`, ~6 tests |
| `crates/agentos-kernel/src/kernel.rs` | Refactor `sanitize_chat_inference_result` to use profile-based pipeline |

### 4. Verification

Same as prior phases plus regression tests confirming that delivery/history/debug profiles produce the expected text for the same input.

## Related

- [[Output Sanitization Plan]]
- [[Output Sanitization Research]] — OpenClaw's profile pattern
- [[05-error-payload-rewriting]] — previous phase
