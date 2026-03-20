---
title: Multipart Upload Support
tags: [tools, network, next-steps]
date: 2026-03-18
status: complete
effort: 45m
priority: medium
---

# Multipart Upload Support

> Add `multipart_fields` parameter to `http-client` for multipart/form-data requests, enabling file uploads to storage APIs and form submissions.

## What to Do

1. Add `multipart` to reqwest features in `crates/agentos-tools/Cargo.toml`
2. In `execute()`, before the `body` check, inspect `multipart_fields: Option<Object>`
3. Build `reqwest::multipart::Form`:
   - String values → `form.text(key, val)`
   - Object values with `base64` key → `reqwest::multipart::Part::bytes(decoded)` with optional `filename` and `content_type`
4. `multipart_fields` takes precedence over `body` if both are present

## Input Schema

```json
{
  "multipart_fields": {
    "field_name": "text_value",
    "file_field": {
      "base64": "<base64-encoded bytes>",
      "filename": "data.bin",
      "content_type": "application/octet-stream"
    }
  }
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/Cargo.toml` | Add `multipart` to reqwest features |
| `crates/agentos-tools/src/http_client.rs` | Add multipart body building |

## Prerequisites

[[30-01-HTTP Client Redirect Control]]

## Verification

`cargo build -p agentos-tools` — must compile with multipart feature.
