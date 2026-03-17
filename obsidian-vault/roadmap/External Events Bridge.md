---
title: External Events Bridge
tags: [roadmap, events, integration]
date: 2026-03-17
status: planned
priority: low
---

# External Events Bridge

> Build an ingestion subsystem that receives external triggers and emits them into the AgentOS kernel event bus.

---

## Problem

Four external event types are fully defined in `crates/agentos-types/src/event.rs` but have no emission infrastructure:

| Event Type | Description |
|---|---|
| `WebhookReceived` | Inbound HTTP webhook payload from a third-party service |
| `ExternalFileChanged` | File change notification from an external filesystem watcher |
| `ExternalAPIEvent` | Generic event received from a polled or push-based external API |
| `ExternalAlertReceived` | Alert or notification from an external monitoring or alerting system |

These types exist in the type system and appear in trigger prompt handling, but no component actually constructs or emits them. As a result, agents cannot react to external stimuli today.

## Proposed Solution

Build an **External Events Bridge** subsystem — a lightweight service (likely running as a kernel background task or a sidecar) that:

1. Listens for inbound HTTP webhooks via a dedicated endpoint (extending `agentos-web` or a standalone Axum listener).
2. Polls or subscribes to configured external API sources on a schedule.
3. Watches filesystem paths via `notify` (inotify on Linux) for external file change events.
4. Receives alert payloads from configured monitoring integrations.
5. Validates, normalizes, and emits each inbound signal as the corresponding typed event into the kernel event bus (`agentos-bus`).

## Current State

- Event types defined: `crates/agentos-types/src/event.rs`
- Event bus infrastructure: `crates/agentos-bus/`
- No bridge component exists; no emission sites for these four event types anywhere in the codebase.

## Scope

- No implementation timeline is set.
- This is lower priority than core agent features (task execution, memory, security hardening).
- Should be planned after the event trigger system is fully wired (see [[Event Trigger Completion Data Flow]]).

## Dependencies

- Requires a stable event emission API in the kernel (in-progress as of 2026-03-17).
- Webhook endpoint requires authentication/HMAC validation to prevent spoofing.
- Filesystem watcher requires capability token enforcement scoped to allowed paths.

## Related

- [[V3 Roadmap]]
- [[Event Trigger Completion Data Flow]]
