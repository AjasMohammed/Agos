---
title: SSE Streaming Response
tags: [tools, network, next-steps]
date: 2026-03-18
status: complete
effort: 1h
priority: high
---

# SSE Streaming Response

> Add `stream: bool` parameter to `http-client` that parses Server-Sent Events and returns them as a structured JSON array instead of raw text.

## What to Do

1. In `execute()` read `stream: bool` from payload (default `false`)
2. When `stream == true`, buffer the full response body (10MB cap) then call `parse_sse_text()`
3. Add `fn parse_sse_text(text: &str, max_events: usize) -> Vec<Value>` that:
   - Splits on `\n\n` to get event blocks
   - Parses `data:`, `event:`, `id:` lines per SSE spec (RFC 8895)
   - Attempts JSON parse of data, falls back to string
   - Returns array capped at 1000 events
4. Return `{ status, headers, events: [...], count, latency_ms }`

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/http_client.rs` | Add `stream` param + `parse_sse_text` fn |

## Prerequisites

[[30-01-HTTP Client Redirect Control]]

## Verification

`cargo test -p agentos-tools` — compile and pass.
