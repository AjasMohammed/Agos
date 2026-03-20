---
title: Wire SsrfAwareDnsResolver into WebFetch and HttpClientTool
tags:
  - security
  - tools
  - v3
  - next-steps
date: 2026-03-19
status: planned
effort: 2h
priority: high
---

# Wire SsrfAwareDnsResolver into WebFetch and HttpClientTool

> Inject `SsrfAwareDnsResolver` into all `reqwest::Client` instances in `web_fetch.rs` and `http_client.rs` via `ClientBuilder::dns_resolver()`, so that every HTTP connection validates resolved IPs against SSRF-unsafe ranges.

---

## Why This Phase

Phase 01 built the resolver in isolation. This phase wires it into the actual HTTP tools that agents use. After this phase, every DNS resolution performed by `WebFetch` or `HttpClientTool` flows through `SsrfAwareDnsResolver`, closing the DNS rebinding vulnerability for both the initial request and all redirect hops.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `WebFetch::new()` | `Client::builder()` with no custom resolver | `Client::builder().dns_resolver(Arc::new(SsrfAwareDnsResolver::new(allow_private)))` |
| `HttpClientTool::new()` | Two `Client::builder()` calls (no-redirect + redirect) with no custom resolver | Both builders use `.dns_resolver(resolver.clone())` |
| `is_private_ip()` in `web_fetch.rs` | Local function definition (lines 69-82) | Removed; imports `crate::ssrf_resolver::is_private_ip` |
| `is_private_ip()` in `http_client.rs` | Local function definition (lines 650-663) | Removed; imports `crate::ssrf_resolver::is_private_ip` |
| Redirect policy SSRF checks | Use local `is_private_ip()` | Use imported `crate::ssrf_resolver::is_private_ip` (or `is_ssrf_unsafe`) |

---

## What to Do

### Step 1: Modify `crates/agentos-tools/src/web_fetch.rs`

1. Add import at the top of the file:

```rust
use crate::ssrf_resolver::SsrfAwareDnsResolver;
use std::sync::Arc;
```

2. In `WebFetch::new()`, read the test-mode flag and inject the resolver. Replace the `Client::builder()` chain (lines 16-62) with:

```rust
pub fn new() -> Result<Self, AgentOSError> {
    let allow_private = std::env::var("AGENTOS_TEST_ALLOW_LOCAL").is_ok();
    let resolver = Arc::new(SsrfAwareDnsResolver::new(allow_private));

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .dns_resolver(resolver)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            // ... existing redirect policy unchanged ...
        }))
        .user_agent("AgentOS/1.0")
        .build()
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "web-fetch".into(),
            reason: format!("Failed to build HTTP client: {}", e),
        })?;

    Ok(Self { client })
}
```

3. Remove the local `is_private_ip()` function definition (lines 69-82). Replace references in the redirect policy with `crate::ssrf_resolver::is_private_ip`.

4. Update the redirect policy closure to import from the shared module. Since the redirect closure is `'static` and cannot capture module-level functions by path, use `crate::ssrf_resolver::is_ssrf_unsafe` directly:

```rust
.redirect(reqwest::redirect::Policy::custom(|attempt| {
    if attempt.previous().len() >= 5 {
        return attempt.error("too many redirects (limit: 5)");
    }
    let block_reason: Option<String> = {
        let url = attempt.url();
        url.host_str().and_then(|host| {
            if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                if ip.is_loopback()
                    || crate::ssrf_resolver::is_ssrf_unsafe(&ip)
                    || ip.is_unspecified()
                    || ip.is_multicast()
                {
                    return Some(format!(
                        "SSRF: redirect to private IP blocked: {}",
                        ip
                    ));
                }
            } else {
                let lower = host.to_lowercase();
                if lower == "localhost"
                    || lower.ends_with(".localhost")
                    || lower.ends_with(".local")
                {
                    return Some(format!(
                        "SSRF: redirect to local hostname blocked: {}",
                        host
                    ));
                }
            }
            None
        })
    };
    if let Some(reason) = block_reason {
        attempt.error(reason)
    } else {
        attempt.follow()
    }
}))
```

Note: The redirect policy closure cannot capture references to crate-level functions with `use` in the enclosing scope because it must be `'static`. Calling `crate::ssrf_resolver::is_ssrf_unsafe` via its fully qualified path works in closures that are `'static`. If the compiler rejects this due to the closure being `move`, define a standalone function `fn redirect_ssrf_check(url: &Url) -> Option<String>` at module scope that calls `crate::ssrf_resolver::is_ssrf_unsafe`.

### Step 2: Modify `crates/agentos-tools/src/http_client.rs`

1. Add imports at the top:

```rust
use crate::ssrf_resolver::SsrfAwareDnsResolver;
```

The file already imports `std::sync::Arc` (via other paths) but verify it is present.

2. In `HttpClientTool::new()`, create the shared resolver and inject it into both clients. The resolver `Arc` is cloned for both builders:

```rust
pub fn new() -> Result<Self, AgentOSError> {
    let allow_private = std::env::var("AGENTOS_TEST_ALLOW_LOCAL").is_ok();
    let resolver = Arc::new(SsrfAwareDnsResolver::new(allow_private));

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("AgentOS/1.0")
        .dns_resolver(resolver.clone())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "http-client".into(),
            reason: format!("Failed to build HTTP client: {}", e),
        })?;

    let client_redirect = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("AgentOS/1.0")
        .dns_resolver(resolver)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            // ... existing redirect policy unchanged, but use
            // crate::ssrf_resolver::is_ssrf_unsafe instead of
            // local is_private_ip ...
        }))
        .build()
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "http-client".into(),
            reason: format!("Failed to build HTTP redirect client: {}", e),
        })?;

    Ok(Self {
        client,
        client_redirect,
    })
}
```

3. Remove the local `is_private_ip()` function definition (lines 650-663 in `http_client.rs`).

4. Update the redirect policy closure to use `crate::ssrf_resolver::is_ssrf_unsafe` (same pattern as `web_fetch.rs` above).

### Step 3: Verify existing tests still pass

The existing SSRF unit tests in `web_fetch.rs` (tests for literal IP blocking and localhost blocking) should continue to pass because the hostname pre-check is retained as defense-in-depth. The `AGENTOS_TEST_ALLOW_LOCAL` behavior is also preserved.

```bash
cargo test -p agentos-tools -- web_fetch --nocapture
cargo test -p agentos-tools -- http_client --nocapture
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/web_fetch.rs` | Add `SsrfAwareDnsResolver` import; inject resolver into `Client::builder()`; remove local `is_private_ip()`; update redirect closure to use shared `is_ssrf_unsafe` |
| `crates/agentos-tools/src/http_client.rs` | Add `SsrfAwareDnsResolver` import; inject resolver into both `Client::builder()` calls; remove local `is_private_ip()`; update redirect closure to use shared `is_ssrf_unsafe` |

---

## Prerequisites

[[01-ssrf-resolver-impl]] must be complete. The `SsrfAwareDnsResolver` struct and `is_ssrf_unsafe()` function must exist in `crates/agentos-tools/src/ssrf_resolver.rs`.

---

## Test Plan

- `cargo test -p agentos-tools -- web_fetch` -- all 7 existing tests pass unchanged
- `cargo test -p agentos-tools -- http_client` -- all existing tests pass unchanged (they use `AGENTOS_TEST_ALLOW_LOCAL` which flows through to the resolver's `allow_private` flag)
- Verify `is_private_ip` no longer exists as a local function in either `web_fetch.rs` or `http_client.rs` (grep for `fn is_private_ip`)
- The resolver's `allow_private: true` mode correctly allows wiremock tests to reach `127.0.0.1`

---

## Verification

```bash
# Build
cargo build -p agentos-tools

# Run all tool tests (web_fetch + http_client + ssrf_resolver)
cargo test -p agentos-tools --nocapture

# Confirm no duplicate is_private_ip definitions
grep -rn "fn is_private_ip" crates/agentos-tools/src/web_fetch.rs crates/agentos-tools/src/http_client.rs
# Expected: no output (function removed from both files)

# Confirm is_private_ip only exists in ssrf_resolver.rs
grep -rn "fn is_private_ip" crates/agentos-tools/src/
# Expected: only ssrf_resolver.rs

# Clippy
cargo clippy -p agentos-tools -- -D warnings
```

---

## Related

- [[DNS SSRF Fix Plan]]
- [[01-ssrf-resolver-impl]]
- [[03-cleanup-and-docs]]
- [[04-integration-tests]]
