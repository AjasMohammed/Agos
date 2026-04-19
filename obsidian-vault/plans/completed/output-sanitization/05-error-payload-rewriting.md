---
title: Phase 5 — Error Payload Rewriting
tags:
  - kernel
  - chat
  - llm
  - security
  - phase
date: 2026-04-12
status: in-progress
effort: 0.5d
priority: medium
---

# Phase 5 — Error Payload Rewriting

> Detect raw provider error payloads (JSON API errors, HTTP status lines, Cloudflare HTML pages, context-overflow messages) leaked into user-visible chat text and replace them with clean, actionable user messages. Mirrors OpenClaw's `formatRawAssistantErrorForUi` pass.

---

## Why this phase

LLM providers sometimes return error responses that the adapter partially processes but still passes raw text to the model or chat layer. Examples seen in production:

- `{"error": {"type": "invalid_request_error", "message": "prompt is too long"}}` — raw Anthropic/OpenAI error JSON
- `Error 429: Too Many Requests` — HTTP status line the adapter logged as text
- `<!DOCTYPE html>...Access denied | Cloudflare...` — Cloudflare challenge page when the provider is behind a CDN
- `context length exceeded` / `maximum context length` — context overflow from the provider

These appear in `result.text` as the "answer" and get rendered in the chat UI as confusing technical gibberish. OpenClaw has a dedicated `formatRawAssistantErrorForUi` function that pattern-matches these and substitutes clean user-facing messages with actionable advice.

## Detailed subtasks

### 1. Add `rewrite_error_payload` to [[../../../crates/agentos-kernel/src/output_sanitizer.rs|output_sanitizer.rs]]

```rust
/// If `text` looks like a raw provider error payload, return a clean
/// user-facing replacement message. Returns `None` when the text does
/// not match any known error pattern (i.e., it's normal prose and
/// should not be touched).
pub fn rewrite_error_payload(text: &str) -> Option<String>;
```

Detection patterns (ordered from most specific to most general):

| Pattern | Detection | Replacement |
|---|---|---|
| JSON API error | Starts with `{` and parses as `{"error": {"type": "...", "message": "..."}}` or `{"error": "..."}` | `"LLM request failed: {message}. Please try again."` |
| HTTP status line | Matches `^(Error\|HTTP)\s*\d{3}[:\s]` | `"LLM request failed (HTTP {code}). Please try again."` |
| Cloudflare HTML | Contains `<!DOCTYPE` or `<html` case-insensitive AND contains `cloudflare` or `access denied` | `"LLM request failed: received an error page from the provider's CDN. Please try again."` |
| Context overflow | Contains `context length exceeded` OR `prompt is too long` OR `maximum context length` OR `request too large` (case-insensitive) | `"Context too large for this model. Try /new to start a fresh session, or switch to a model with a larger context window."` |
| Rate limit | Contains `rate limit` or `too many requests` (case-insensitive) | `"LLM request was rate-limited. Please wait a moment and try again."` |

Only rewrite when the text is **primarily** an error (not just mentioning the word "error" in a normal sentence). Heuristic: text length < 2 KB and matches one of the patterns above. Normal assistant prose is typically longer and doesn't start with `{` or `<!DOCTYPE`.

### 2. Wire into `sanitize_chat_inference_result`

Apply `rewrite_error_payload` to the `visible_text` string **after** all other sanitization passes. If it returns `Some(replacement)`, use the replacement as `visible_text`. The raw `result.text` in the context window is unaffected — the model should see the original error so it can reason about retrying.

### 3. Files changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/output_sanitizer.rs` | Add `rewrite_error_payload` + pattern helpers + ~12 unit tests |
| `crates/agentos-kernel/src/kernel.rs` | Apply `rewrite_error_payload` to `visible_text` in `sanitize_chat_inference_result` |

### 4. Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel --lib output_sanitizer::
cargo test -p agentos-kernel --test e2e chat_tool_loop
cargo clippy -p agentos-kernel -p agentos-audit -- -D warnings
cargo fmt -p agentos-kernel -- --check
```

## Related

- [[Output Sanitization Plan]]
- [[Output Sanitization Research]] — OpenClaw Layer E
- [[04-code-region-awareness]] — previous phase
- [[06-profile-based-sanitizer]] — next phase
