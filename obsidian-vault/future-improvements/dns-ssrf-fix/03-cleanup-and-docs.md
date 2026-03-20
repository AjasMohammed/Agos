---
title: SSRF Resolver Cleanup and Documentation
tags:
  - security
  - tools
  - v3
  - next-steps
date: 2026-03-19
status: planned
effort: 1h
priority: medium
---

# SSRF Resolver Cleanup and Documentation

> Clean up residual SSRF-related comments, document the hostname pre-check as defense-in-depth, and update the agent-manual tool's permissions section to describe the DNS-level SSRF protection.

---

## Why This Phase

After phases 01 and 02, the DNS rebinding vulnerability is closed. This phase addresses documentation and code hygiene:

1. The existing hostname pre-check in both tools should be clearly documented as defense-in-depth (not the primary enforcement layer).
2. Any `TODO` comments about DNS rebinding should be removed since the issue is now fixed.
3. The `agent-manual` tool generates documentation that agents read to understand their capabilities and constraints. Its permissions/security section should describe the SSRF protection so agents understand they cannot fetch private URLs even via DNS tricks.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Hostname pre-check comments | No comment explaining it is defense-in-depth | Comment block explains this is a fast-path check; the authoritative layer is `SsrfAwareDnsResolver` |
| DNS rebinding TODO comments | May exist (search for `TODO: DNS`, `TODO: rebind`, `FIXME: SSRF`) | Removed -- the fix is implemented |
| Agent manual SSRF documentation | Does not mention DNS-level protection | Permissions section describes that all resolved IPs are validated |
| `AGENTOS_TEST_ALLOW_LOCAL` documentation | Inline warning log only | Add a doc comment on `SsrfAwareDnsResolver::new()` explaining the flag |

---

## What to Do

### Step 1: Add defense-in-depth comments to `web_fetch.rs`

Open `crates/agentos-tools/src/web_fetch.rs`. Locate the SSRF protection block (around line 137). Add a comment block:

```rust
// SSRF defense-in-depth: fast-path hostname check.
//
// This rejects obvious SSRF attempts (literal private IPs, localhost,
// .local domains) before making a network request. It is NOT the
// primary enforcement layer -- the `SsrfAwareDnsResolver` injected
// into the reqwest Client validates all DNS-resolved IPs at connection
// time, closing the DNS rebinding gap. This check remains for:
//   1. Instant rejection without a DNS lookup for trivial cases
//   2. Defense-in-depth if the resolver is misconfigured
```

### Step 2: Add defense-in-depth comments to `http_client.rs`

Open `crates/agentos-tools/src/http_client.rs`. Locate the SSRF protection block (around line 152). Add the same comment block as Step 1.

### Step 3: Remove any DNS rebinding TODO/FIXME comments

Search across all files in `crates/agentos-tools/src/` for comments mentioning DNS rebinding, TOCTOU, or SSRF TODO markers:

```bash
grep -rn "TODO.*DNS\|TODO.*rebind\|TODO.*SSRF\|FIXME.*SSRF\|TOCTOU" crates/agentos-tools/src/
```

Remove any found comments that reference the vulnerability as unfixed. Replace with a brief note referencing the resolver if context is helpful:

```rust
// DNS rebinding protection: handled by SsrfAwareDnsResolver (see ssrf_resolver.rs)
```

### Step 4: Update agent-manual tool permissions documentation

Open `crates/agentos-tools/src/agent_manual.rs`. Locate the section that generates documentation about network permissions or security constraints. The `agent-manual` tool has a `ManualSection` enum with sections like `Permissions`, `Security`, or `Tools`.

Find the section that describes `network.outbound` permissions and add a paragraph:

```
## Network SSRF Protection

All outbound HTTP requests (via `web-fetch` and `http-client` tools) are
protected against Server-Side Request Forgery (SSRF) at two layers:

1. **Hostname pre-check** -- Literal private IPs (127.0.0.1, 192.168.x.x,
   169.254.x.x, etc.) and local hostnames (localhost, *.local) are rejected
   immediately before any network request.

2. **DNS-level validation** -- A custom DNS resolver validates every resolved
   IP address against private/loopback/link-local ranges. This prevents DNS
   rebinding attacks where an attacker-controlled domain resolves to a private
   IP after passing hostname checks.

Blocked IP ranges: loopback (127.0.0.0/8), RFC1918 private (10.0.0.0/8,
172.16.0.0/12, 192.168.0.0/16), link-local (169.254.0.0/16 including cloud
metadata 169.254.169.254), multicast, unspecified, and IPv6 equivalents.
```

If `agent_manual.rs` uses a match on `ManualSection` variants to generate text, add this content to the relevant arm (likely `Permissions` or `Security`).

### Step 5: Verify

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools
cargo clippy -p agentos-tools -- -D warnings
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/web_fetch.rs` | Add defense-in-depth comment block above SSRF hostname check |
| `crates/agentos-tools/src/http_client.rs` | Add defense-in-depth comment block above SSRF hostname check |
| `crates/agentos-tools/src/agent_manual.rs` | Add SSRF protection documentation to the permissions/security section |

---

## Prerequisites

[[02-wire-resolver-into-clients]] must be complete. The resolver is wired in and `is_private_ip()` is consolidated.

---

## Test Plan

- `cargo build -p agentos-tools` compiles cleanly
- `cargo test -p agentos-tools` -- all tests pass (no logic changes in this phase)
- `cargo clippy -p agentos-tools -- -D warnings` -- no new warnings
- Manual review: grep confirms no remaining `TODO.*DNS` or `TODO.*SSRF` comments in `crates/agentos-tools/src/`
- Manual review: `agent_manual.rs` output includes "DNS-level validation" text when the permissions section is queried

---

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools
cargo clippy -p agentos-tools -- -D warnings

# Confirm no remaining SSRF TODO comments
grep -rn "TODO.*DNS\|TODO.*rebind\|TODO.*SSRF\|FIXME.*SSRF" crates/agentos-tools/src/
# Expected: no output

# Confirm defense-in-depth comments exist
grep -n "defense-in-depth" crates/agentos-tools/src/web_fetch.rs crates/agentos-tools/src/http_client.rs
# Expected: at least one hit per file
```

---

## Related

- [[DNS SSRF Fix Plan]]
- [[02-wire-resolver-into-clients]]
- [[04-integration-tests]]
