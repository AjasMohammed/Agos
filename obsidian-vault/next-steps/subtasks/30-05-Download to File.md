---
title: Download to File
tags: [tools, network, next-steps]
date: 2026-03-18
status: complete
effort: 30m
priority: medium
---

# Download to File

> Add `save_to` parameter to `http-client` that streams the response body directly to a file in `data_dir`, bypassing the 10MB memory cap.

## What to Do

1. In `execute()`, read `save_to: Option<&str>` from payload
2. Block path traversal: reject any `save_to` containing `..`
3. When set, stream response chunks directly to `tokio::fs::File` using `AsyncWriteExt::write_all`
4. Return `{ status, headers, saved_to, size_bytes, content_type, latency_ms }` without body field
5. `save_to` path is relative to `context.data_dir`; create parent dirs if needed

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/http_client.rs` | Add `save_to` streaming path |

## Prerequisites

[[30-01-HTTP Client Redirect Control]]

## Verification

`cargo test -p agentos-tools` — compile and pass.
