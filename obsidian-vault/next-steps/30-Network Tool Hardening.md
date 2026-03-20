---
title: Network Tool Hardening for Pure Agentic Workflow
tags:
  - tools
  - network
  - next-steps
  - v3
date: 2026-03-18
status: in-progress
effort: 4h
priority: high
---

# Network Tool Hardening for Pure Agentic Workflow

> Close five critical gaps in the network tool layer that prevent agents from performing real-world API workflows: redirect following, SSE streaming, missing network-monitor manifest, multipart upload, and download-to-file.

---

## Current State

| Gap | Impact |
|-----|--------|
| No redirect following | OAuth, many REST APIs fail silently with 3xx |
| No SSE/streaming | All streaming LLM API calls (OpenAI, Anthropic) are broken |
| No `network-monitor.toml` | Tool is invisible to dynamic tool discovery |
| No multipart upload | Can't push files to storage APIs or forms |
| No download-to-file | Large file downloads blocked by 10MB memory cap |

## Goal / Target State

`http-client` accepts five new optional parameters:
- `follow_redirects: bool` — enables up to 10 redirect hops (OAuth, CDN)
- `stream: bool` — SSE mode: returns `events[]` array of parsed events instead of raw body
- `multipart_fields: object` — builds a multipart/form-data request body
- `save_to: string` — streams response directly to a file in `data_dir`, no memory cap

`network-monitor` has a TOML manifest and is discoverable via the tool registry.

## Sub-tasks

| # | Task | File | Status |
|---|------|------|--------|
| 01 | [[30-01-HTTP Client Redirect Control]] | `http_client.rs` | complete |
| 02 | [[30-02-SSE Streaming Response]] | `http_client.rs` | complete |
| 03 | [[30-03-Network Monitor TOML Manifest]] | `tools/core/network-monitor.toml` | complete |
| 04 | [[30-04-Multipart Upload]] | `http_client.rs`, `Cargo.toml` | complete |
| 05 | [[30-05-Download to File]] | `http_client.rs` | complete |

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/http_client.rs` | Add redirect client, SSE parser, multipart body, file download |
| `crates/agentos-tools/Cargo.toml` | Add `multipart` to reqwest features |
| `tools/core/network-monitor.toml` | Create missing manifest |
| `tools/core/http-client.toml` | Update description |

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools
cargo clippy -p agentos-tools -- -D warnings
```

## Related

[[29-File Operations Expansion]], [[22-Unwired Features]]
