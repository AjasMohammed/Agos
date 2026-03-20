---
title: "Pipeline Template Variable Sanitization"
tags:
  - next-steps
  - security
  - pipeline
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 4h
priority: critical
---

# Pipeline Template Variable Sanitization

> Add context-aware escaping for pipeline template variables to prevent JSON injection and prompt injection.

## What to Do

In `crates/agentos-pipeline/src/engine.rs`, template variables (`{{var}}`) are interpolated without any escaping into JSON tool inputs and LLM prompts. If a step's output contains quotes, brackets, or prompt injection patterns, downstream steps break or become injection vectors.

**Example attack:**
```yaml
steps:
  - id: step1
    task: "Write report"
    output_var: report
  - id: step2
    tool: save-file
    input:
      content: "{{report}}"  # If report = 'foo","malicious":true,"x":"', JSON is corrupted
```

### Steps

1. **Identify all interpolation sites** in `engine.rs`:
   - Find where `{{var}}` replacement happens
   - Classify each site as: JSON context, prompt context, or raw context

2. **Add sanitization functions:**
   ```rust
   /// Escape for JSON string context: escape quotes, backslashes, control chars
   fn sanitize_for_json(value: &str) -> String {
       serde_json::to_string(value)
           .unwrap_or_default()
           .trim_matches('"')
           .to_string()
   }

   /// Escape for prompt context: wrap in <user_data> tags, strip injection patterns
   fn sanitize_for_prompt(value: &str) -> String {
       format!("<user_data>{}</user_data>", value)
   }
   ```

3. **Apply context-aware escaping:**
   - When interpolating into a JSON string value: use `sanitize_for_json()`
   - When interpolating into a prompt/task description: use `sanitize_for_prompt()`
   - When used as a raw variable (not inside a string): validate it's a safe type (number, boolean, or pre-sanitized string)

4. **Add opt-out for trusted variables:**
   - Built-in variables (`run_id`, `date`, `timestamp`) are kernel-generated and safe — skip sanitization
   - User variables from step outputs: always sanitize

5. **Add tests** for injection attempts

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-pipeline/src/engine.rs` | Add `sanitize_for_json()`, `sanitize_for_prompt()`, apply at interpolation sites |

## Prerequisites

None — independent security fix.

## Verification

```bash
cargo test -p agentos-pipeline
cargo clippy --workspace -- -D warnings
```

Test: step output containing `","evil":true,"` → downstream JSON is valid and does not contain injected fields. Step output containing `ignore previous instructions` → wrapped in `<user_data>` tags.
