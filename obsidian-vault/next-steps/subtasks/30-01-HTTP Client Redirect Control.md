---
title: HTTP Client Redirect Control
tags: [tools, network, next-steps]
date: 2026-03-18
status: complete
effort: 30m
priority: high
---

# HTTP Client Redirect Control

> Add `follow_redirects` parameter to `http-client` so agents can handle OAuth flows, CDN redirects, and shortened URLs.

## What to Do

1. Open `crates/agentos-tools/src/http_client.rs`
2. Add `client_redirect: Client` field to `HttpClientTool` built with `Policy::limited(10)`
3. In `execute()`, read `follow_redirects: bool` from payload (default `false`)
4. Select `&self.client_redirect` or `&self.client` based on flag

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/http_client.rs` | Add second client field + selection logic |

## Prerequisites

None — standalone change.

## Verification

`cargo test -p agentos-tools` — all existing tests pass.
