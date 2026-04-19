---
title: Mobile App Research
tags:
  - mobile
  - research
  - plan
date: 2026-04-19
status: complete
effort: research-only
priority: high
---

# Mobile App Research

> Stack selection, auth pattern comparison, push-delivery options, and competitive landscape for an AgentOS mobile client.

---

## Goal

Pick a stack and a set of architectural defaults that let a small team ship a production-quality mobile client that exercises the AgentOS feature surface (chat, tasks, workflows, approvals) with minimum rewrite risk.

## Stack comparison

| Stack | Pros | Cons | Verdict |
|-------|------|------|---------|
| **React Native + TS + Expo** | Largest ecosystem, JS/TS is already team's web language, Expo EAS handles native builds/signing, first-class push (Expo Notifications), SSE and WebSocket libs mature, easy OAuth (`expo-auth-session`) | JS bridge overhead vs native, occasional EAS build quota limits, some native modules need ejection | **Chosen** |
| Flutter + Dart | Excellent perf, consistent UI, strong widget library | Dart is a net-new language for the team, smaller ecosystem for OAuth/SSE, native package gaps (Ed25519 on iOS awkward) | Runner-up |
| Native (Swift + Kotlin) | Best perf and platform fidelity | 2× work (two codebases), slowest iteration, overkill for a thin client | Rejected |
| Capacitor/Ionic (web in a shell) | Reuse WebUI code | Poor push integration, sluggish UX, uncanny-valley feel on iOS | Rejected |
| Tauri Mobile | Rust end-to-end, code reuse with kernel types | Still beta for mobile, small community, signing pipelines immature | Rejected (revisit in 12 months) |

**Decision:** React Native with Expo Managed Workflow for v1. Start pure-managed and only eject if a native module requires it.

## Auth pattern comparison

| Pattern | Mobile UX | Security | Revocation | Verdict |
|---------|-----------|----------|------------|---------|
| **OAuth2 Authorization Code + PKCE + JWT (short-lived) + refresh token** | Good — in-app browser flow, biometric unlock on app resume | Strong; access token is short-lived, refresh rotates | Refresh-token revocation list on server | **Chosen** |
| Long-lived HMAC API keys (current) | Bad — requires copy-paste | Acceptable if stored in Keychain | Manual — revoke entire key | Keep for CLI, not mobile |
| Device certs / mTLS | Strong | Strong | Cert revocation | Deferred — valuable later for IoT/robotics clients, not human mobile UX |
| Magic-link email | Great UX | Weak — email compromise = account takeover | Expire link | Rejected — escalations demand stronger auth |
| Passkeys (WebAuthn / Passkeys) | Great UX | Strong | Per-credential | Stretch goal for v2; requires platform libs and WebAuthn server-side |

**Decision:** OAuth2 Authorization Code + PKCE. Access token: JWT, 15 min, RS256, signed by an Ed25519-over-JWS keypair stored in the vault. Refresh token: 30 days, rotating, HMAC-signed, stored in server-side SQLite with revocation. Existing `agentos-api` HMAC key flow is untouched — mobile adds a new `/v1/auth/*` namespace.

## Push delivery

| Option | Setup | Cost | Notes |
|--------|-------|------|-------|
| **Expo Push Service** | Trivial; no Apple/Google keys needed in dev | Free | Default for v1; Expo handles APNs + FCM behind a single API |
| Direct APNs (HTTP/2) | Apple Developer acct, p8 key, cert rotation | Apple dev $99/yr | Add as a config-selectable transport in prod |
| Direct FCM (HTTP v1) | Google Cloud project, service account JSON | Free at our scale | Pair with APNs for dual-platform prod |
| OneSignal / Pusher Beams | SaaS — extra vendor | Free tier | Rejected — we already have a channel abstraction |

**Decision:** `MobilePushAdapter` implements a `PushTransport` trait with `ExpoPushTransport` (default) and `ApnsFcmTransport` (production) impls. Config selects transport.

## SSE vs WebSocket for chat

- Existing `agentos-api` already streams chat via SSE (`/v1/chat/completions` with `stream: true`).
- SSE is simpler (unidirectional, HTTP/2 multiplexable, reconnect baked in via `Last-Event-ID`), fits the chat-token-stream use case perfectly.
- WebSockets would be necessary if we wanted bidirectional tool interrupts mid-stream; we don't, for v1.
- iOS Safari / network proxies handle SSE fine over TLS 1.3.

**Decision:** SSE for chat. WebSocket only if a future feature demands it.

## Pipeline builder UX reference

Surveyed builders:
- **n8n** — node-graph, desktop-first, not mobile-friendly
- **Zapier mobile** — linear step list, works well on phone
- **Shortcuts (iOS)** — block list, drag-to-reorder
- **Make.com** — canvas editor, unusable on phone

**Decision:** Adopt the Zapier/Shortcuts linear-step pattern on mobile. The underlying `PipelineDefinition` type already supports DAGs, but v1 mobile UI is restricted to linear pipelines. Web builder keeps the DAG canvas.

## Comparable products

| Product | Relevance | Lessons |
|---------|-----------|---------|
| Claude mobile app | Same chat-with-agent primitive | Streaming UX bar; token-by-token render with code block buffering |
| ChatGPT mobile | Ditto | Voice input is sticky (out of scope for v1, plan hook) |
| Linear mobile | Task-list gold standard | Swipe actions, offline-queued actions (we defer offline) |
| 1Password mobile | Good OAuth2 UX + secure local storage | Use Keychain/Keystore via `expo-secure-store` |
| PagerDuty mobile | Approval-under-pressure UX | High-priority notifications + Face ID confirm (v2 hardening) |

## Libraries shortlist

| Need | Library | Version | Notes |
|------|---------|---------|-------|
| App framework | `expo` | ≥52 | Managed workflow |
| Navigation | `expo-router` | latest | File-based routing |
| Auth | `expo-auth-session` + `expo-secure-store` | latest | PKCE out-of-the-box |
| HTTP / SSE | `@microsoft/fetch-event-source` | ^2 | Robust SSE reconnect |
| State | `zustand` or `@tanstack/react-query` | latest | RQ for server state, zustand for UI |
| Types | Generated from OpenAPI via `openapi-typescript` | latest | Single source of truth |
| UI | NativeWind (Tailwind for RN) | latest | Fast styling, matches web stack mindset |
| Forms | `react-hook-form` + `zod` | latest | Schema-validated forms |
| Push | `expo-notifications` | latest | Dev + prod |
| Testing | Jest + React Native Testing Library + Maestro (E2E) | latest | Maestro runs on CI via MCP |

## OpenAPI generation

Plan to add `utoipa` or `aide` to `agentos-api` so we can emit an `openapi.json` at build time; mobile consumes it via `openapi-typescript` to produce a fully typed client. This also benefits web UI, CLI HTTP wrappers, and external integrators.

## Related

- [[Mobile App Plan]]
- [[Mobile App Data Flow]]
- [[WebUI Redesign Research]]
