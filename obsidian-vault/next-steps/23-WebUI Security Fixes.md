---
title: WebUI Security Fixes
tags:
  - webui
  - security
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 5d
priority: critical
---

# WebUI Security Fixes

> Fix 6 critical security vulnerabilities, 8 correctness bugs, and 5 architectural issues in the `agentos-web` crate discovered during code review.

---

## Current State

The `agentos-web` crate provides an HTTP admin UI for the AgentOS kernel. Code review found: all endpoints unauthenticated (C2), CORS wide open to any origin (C1), no CSRF protection (C3), secrets handled as plain `String` never zeroized (C5), tool install reads arbitrary filesystem paths with trivially bypassable `..` check (C6), MiniJinja auto-escape never tested (C4), SSE stream uses count-based tracking that loses events beyond 50 and freezes on deletion (I1), pipeline run handler error handling is minimal (I2), dead `is_partial()` code (I3), vault passphrase leaked via CLI arg in `/proc` (I4), no graceful shutdown (I5), static file path breaks outside workspace root (I6), unbounded audit query limit enables DoS (I7), secret scope silently always `Global` (I8), mutation handlers bypass kernel dispatch skipping audit (S3), no CSP header (S4), no rate limiting (S5), template markup duplicated without `{% include %}` (S1).

## Goal / Target State

All 19 issues fixed across 8 phases. The web crate will have: bearer token + session cookie auth, CSRF tokens in all forms, CORS restricted to bound address, CSP + X-Frame-Options headers, rate limiting via `tower_governor`, `ZeroizingString` for secrets at the HTTP boundary, canonicalized path-allowlisted tool install, ID-based SSE streaming with monotonic row IDs, improved pipeline error handling, and all mutation handlers routed through kernel command dispatch for audit trail consistency.

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[01-quick-wins]] | `handlers/mod.rs`, `handlers/audit.rs`, `handlers/secrets.rs`, 6 template files | planned |
| 02 | [[02-cli-and-startup]] | `commands/web.rs`, `main.rs`, `server.rs`, `router.rs`, 2 Cargo.toml | planned |
| 03 | [[03-cors-auth-csp-ratelimit]] | `router.rs`, `auth.rs` (new), `lib.rs`, `server.rs`, `Cargo.toml` | planned |
| 04 | [[04-csrf-protection]] | `csrf.rs` (new), `state.rs`, `router.rs`, `base.html`, all 7 handlers | planned |
| 05 | [[05-xss-and-secrets]] | `templates.rs`, `handlers/secrets.rs`, `tests/xss_tests.rs` (new) | planned |
| 06 | [[06-tool-install-path-security]] | `state.rs`, `server.rs`, `handlers/tools.rs`, `commands/web.rs` | planned |
| 07 | [[07-sse-and-pipeline-execution]] | `agentos-audit/src/log.rs`, `handlers/tasks.rs`, `handlers/pipelines.rs` | planned |
| 08 | [[08-kernel-dispatch-integration]] | `kernel.rs`, `handlers/agents.rs`, `handlers/tools.rs`, `handlers/secrets.rs` | planned |

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings

# Verify CORS no longer allows any origin
grep -rn "allow_origin(Any)" crates/agentos-web/
# Expected: 0 matches

# Verify auth middleware exists
test -f crates/agentos-web/src/auth.rs && echo "auth.rs exists"

# Verify all mutations go through kernel dispatch
grep -rn "agent_registry.write()\|tool_registry.write()\|vault.set(\|vault.revoke(" crates/agentos-web/src/handlers/
# Expected: 0 matches in mutation handlers

# Verify SSE uses ID-based tracking
grep -c "last_count" crates/agentos-web/src/handlers/tasks.rs
# Expected: 0

# Verify ZeroizingString used for secrets
grep -n "ZeroizingString" crates/agentos-web/src/handlers/secrets.rs
# Expected: at least 1 match
```

## Related

- [[WebUI Security Fixes Plan]] -- master plan with architecture, design decisions, risks
- [[WebUI Security Fixes Data Flow]] -- before/after request flow diagrams
- [[22-Unwired Features]] -- parent tracking issue that identified these gaps
