---
title: "TODO: Update Stale WebUI Security Fixes Plan Statuses"
tags:
  - webui
  - security
  - documentation
  - next-steps
date: 2026-03-17
status: complete
effort: 15m
priority: medium
---

# Update Stale WebUI Security Fixes Plan Statuses

> Update frontmatter `status:` in all WebUI Security Fixes plan files from `planned` to `complete` — all 8 phases are fully implemented but no docs were updated.

## Why This Phase

A plan audit (2026-03-17) confirmed that all 8 phases of the WebUI Security Fixes plan are fully implemented in code:
- Phase 01 (quick wins): audit limit capped, secret scope arms fixed
- Phase 02 (CLI/startup): ZeroizingString passphrase, CancellationToken shutdown, CARGO_MANIFEST_DIR static path
- Phase 03 (CORS/Auth/CSP/Rate Limit): CorsLayer bound to address; `require_auth` middleware; CSP in `add_security_headers()`; GovernorLayer
- Phase 04 (CSRF): `csrf.rs` with session tokens, DashMap storage, TOKEN_TTL
- Phase 05 (XSS/Secrets): ZeroizingString in secrets.rs handler
- Phase 06 (Tool path security): `canonicalize()` + `allowed_tool_dirs` allowlist
- Phase 07 (SSE fix): monotonic ID-based tracking via `query_since_for_task()`
- Phase 08 (Kernel dispatch): `api_connect_agent/disconnect/install_tool/set_secret/revoke_secret` on Kernel
- Template deduplication: partials directory exists; templates use include/extends

All plan files still say `status: planned`.

## Current → Target State

| File | Current | Target |
|------|---------|--------|
| `WebUI Security Fixes Plan.md` | `planned` | `complete` |
| `01-quick-wins.md` | `planned` | `complete` |
| `02-cli-and-startup.md` | `planned` | `complete` |
| `03-cors-auth-csp-ratelimit.md` | `planned` | `complete` |
| `04-csrf-protection.md` | `planned` | `complete` |
| `05-xss-and-secrets.md` | `planned` | `complete` |
| `06-tool-install-path-security.md` | `planned` | `complete` |
| `07-sse-and-pipeline-execution.md` | `planned` | `complete` |
| `08-kernel-dispatch-integration.md` | `planned` | `complete` |
| `TODO-template-deduplication.md` | `planned` | `complete` |

## Detailed Subtasks

1. For each file in the list above, open it and change `status: planned` to `status: complete` in the YAML frontmatter.

2. Also update `WebUI Security Fixes Data Flow.md` if it says `planned` (check first).

## Files Changed

| File | Change |
|------|--------|
| All 9 files listed above | `status: planned` → `status: complete` |

## Dependencies

None — documentation-only change.

## Verification

```bash
grep "^status:" obsidian-vault/plans/webui-security-fixes/*.md
# Expected: all complete
```

## Related

- [[WebUI Security Fixes Plan]] — master plan
- [[audit_report]] — plan audit that identified this gap
