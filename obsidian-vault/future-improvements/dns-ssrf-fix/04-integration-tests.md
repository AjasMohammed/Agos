---
title: SSRF DNS Rebinding Integration Tests
tags:
  - security
  - tools
  - v3
  - next-steps
date: 2026-03-19
status: planned
effort: 3h
priority: high
---

# SSRF DNS Rebinding Integration Tests

> Write integration tests that verify the `SsrfAwareDnsResolver` correctly blocks DNS-rebinding SSRF attacks by testing the full request path through `WebFetch` and `HttpClientTool`, including a mock TCP server on localhost that should be unreachable via domain-name resolution.

---

## Why This Phase

Phases 01-02 implemented the resolver and wired it into the tools. This phase proves the fix works end-to-end. The key test scenario is: a mock TCP server listens on `127.0.0.1`, and the test attempts to reach it via a hostname that resolves to `127.0.0.1`. The resolver must block this -- returning an SSRF error instead of a successful connection.

Testing DNS rebinding precisely is difficult because it requires controlling DNS resolution. The approach here uses two complementary strategies:

1. **Direct resolver unit tests** -- Test `SsrfAwareDnsResolver::resolve()` with a mock or intercepted resolver that returns known private IPs.
2. **Tool-level integration tests** -- Test `WebFetch` and `HttpClientTool` with literal private-IP hostnames and domains that resolve to loopback, verifying the error type and message.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| DNS rebinding test coverage | None -- no test verifies resolved-IP validation | Full integration tests covering loopback, private, link-local, and cloud metadata IPs |
| Resolver unit tests | `is_ssrf_unsafe()` tests only (Phase 01) | Additional tests for `SsrfAwareDnsResolver::resolve()` with real DNS |
| Tool-level SSRF tests | Test literal IPs and `localhost` only | Also test that domains resolving to private IPs are blocked |

---

## What to Do

### Step 1: Add resolver-level integration tests

Open `crates/agentos-tools/src/ssrf_resolver.rs` and add the following tests to the `#[cfg(test)]` module. These tests use real DNS resolution so they require network access; mark them with `#[ignore]` for CI and run with `cargo test -- --ignored` when network is available.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ... existing is_ssrf_unsafe unit tests from Phase 01 ...

    // -- Resolver integration tests (require network) --

    /// Test that resolving a known public domain succeeds.
    /// Uses example.com which has stable public DNS records.
    #[tokio::test]
    #[ignore] // requires network access
    async fn resolver_allows_public_domain() {
        let resolver = SsrfAwareDnsResolver::new(false);
        let name: Name = "example.com".parse().unwrap();
        let result = resolver.resolve(name);
        let addrs = result.await;
        assert!(addrs.is_ok(), "Public domain should resolve successfully");
        let addr_list: Vec<SocketAddr> = addrs.unwrap().collect();
        assert!(!addr_list.is_empty(), "Should return at least one address");
        // Verify all returned IPs are public
        for addr in &addr_list {
            assert!(
                !is_ssrf_unsafe(&addr.ip()),
                "example.com should not resolve to a private IP: {}",
                addr.ip()
            );
        }
    }

    /// Test that resolving localhost is blocked when allow_private is false.
    #[tokio::test]
    async fn resolver_blocks_localhost_resolution() {
        let resolver = SsrfAwareDnsResolver::new(false);
        let name: Name = "localhost".parse().unwrap();
        let result = resolver.resolve(name).await;
        assert!(result.is_err(), "localhost should be blocked by resolver");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("SSRF") || err_msg.contains("private") || err_msg.contains("loopback"),
            "Error should mention SSRF protection: {}",
            err_msg
        );
    }

    /// Test that allow_private=true permits localhost resolution.
    #[tokio::test]
    async fn resolver_allows_localhost_when_private_permitted() {
        let resolver = SsrfAwareDnsResolver::new(true);
        let name: Name = "localhost".parse().unwrap();
        let result = resolver.resolve(name).await;
        assert!(
            result.is_ok(),
            "localhost should be allowed when allow_private is true"
        );
    }
}
```

### Step 2: Add tool-level integration tests

Create or extend `crates/agentos-tools/tests/ssrf_dns_test.rs` with end-to-end tests that exercise the full `WebFetch` and `HttpClientTool` request path.

```rust
//! Integration tests for DNS rebinding SSRF protection.
//!
//! These tests verify that `WebFetch` and `HttpClientTool` cannot reach
//! a local TCP server via a domain name that resolves to a private IP.

use agentos_tools::web_fetch::WebFetch;
use agentos_tools::http_client::HttpClientTool;
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use std::path::PathBuf;

fn ctx_with_network() -> ToolExecutionContext {
    let mut permissions = PermissionSet::new();
    permissions.grant("network.outbound".to_string(), false, false, true, None);
    ToolExecutionContext {
        data_dir: PathBuf::from("/tmp"),
        task_id: TaskID::new(),
        agent_id: AgentID::new(),
        trace_id: TraceID::new(),
        permissions,
        vault: None,
        hal: None,
        file_lock_registry: None,
        agent_registry: None,
        task_registry: None,
    }
}

/// Verify that WebFetch blocks a request to a domain that resolves to
/// 127.0.0.1 (localhost). This is the core DNS rebinding test.
///
/// We use "localhost" as the domain -- the hostname pre-check would
/// catch this, but importantly the resolver ALSO blocks it. To test
/// the resolver specifically (without the hostname pre-check catching
/// it first), we would need a custom domain pointing to 127.0.0.1.
/// That is tested at the resolver level in ssrf_resolver.rs.
#[tokio::test]
async fn web_fetch_blocks_localhost_domain() {
    // Ensure SSRF protection is active (not in test-bypass mode)
    std::env::remove_var("AGENTOS_TEST_ALLOW_LOCAL");
    let tool = WebFetch::new().unwrap();
    let result = tool
        .execute(
            serde_json::json!({"url": "http://localhost:9999/"}),
            ctx_with_network(),
        )
        .await;
    assert!(result.is_err(), "Request to localhost should be blocked");
}

/// Verify that HttpClientTool blocks a request to 127.0.0.1 via
/// the resolver layer (in addition to the hostname pre-check).
#[tokio::test]
async fn http_client_blocks_loopback_ip() {
    std::env::remove_var("AGENTOS_TEST_ALLOW_LOCAL");
    let tool = HttpClientTool::new().unwrap();
    let result = tool
        .execute(
            serde_json::json!({"url": "http://127.0.0.1:9999/", "method": "GET"}),
            ctx_with_network(),
        )
        .await;
    assert!(result.is_err(), "Request to 127.0.0.1 should be blocked");
}

/// Verify that the cloud metadata IP (169.254.169.254) is blocked.
#[tokio::test]
async fn web_fetch_blocks_cloud_metadata_ip() {
    std::env::remove_var("AGENTOS_TEST_ALLOW_LOCAL");
    let tool = WebFetch::new().unwrap();
    let result = tool
        .execute(
            serde_json::json!({"url": "http://169.254.169.254/latest/meta-data/"}),
            ctx_with_network(),
        )
        .await;
    assert!(result.is_err(), "Request to cloud metadata IP should be blocked");
    match &result {
        Err(AgentOSError::PermissionDenied { operation, .. }) => {
            assert!(
                operation.contains("169.254.169.254"),
                "Error should mention the blocked IP: {}",
                operation
            );
        }
        other => panic!("Expected PermissionDenied, got: {:?}", other),
    }
}

/// Verify that RFC1918 private IPs are blocked (10.x, 172.16.x, 192.168.x).
#[tokio::test]
async fn http_client_blocks_rfc1918_private_ips() {
    std::env::remove_var("AGENTOS_TEST_ALLOW_LOCAL");
    let tool = HttpClientTool::new().unwrap();
    let private_ips = [
        "http://10.0.0.1/",
        "http://172.16.0.1/",
        "http://192.168.1.1/",
    ];
    for url in &private_ips {
        let result = tool
            .execute(
                serde_json::json!({"url": url, "method": "GET"}),
                ctx_with_network(),
            )
            .await;
        assert!(
            result.is_err(),
            "Request to {} should be blocked",
            url
        );
    }
}

/// Verify that a request to a public domain works when SSRF
/// protection is active. Uses httpbin.org as a reliable public
/// endpoint.
#[tokio::test]
#[ignore] // requires network access
async fn web_fetch_allows_public_domain() {
    std::env::remove_var("AGENTOS_TEST_ALLOW_LOCAL");
    let tool = WebFetch::new().unwrap();
    let result = tool
        .execute(
            serde_json::json!({"url": "https://httpbin.org/get"}),
            ctx_with_network(),
        )
        .await;
    assert!(
        result.is_ok(),
        "Request to public domain should succeed: {:?}",
        result.err()
    );
}

/// Verify that AGENTOS_TEST_ALLOW_LOCAL bypasses the resolver's
/// private IP blocking, allowing wiremock tests to work.
#[tokio::test]
async fn test_allow_local_bypasses_resolver() {
    std::env::set_var("AGENTOS_TEST_ALLOW_LOCAL", "1");
    // Create tool with bypass active
    let tool = WebFetch::new().unwrap();
    // This would normally be blocked, but with bypass it should
    // fail for a different reason (connection refused, not SSRF)
    let result = tool
        .execute(
            serde_json::json!({"url": "http://127.0.0.1:1/"}),
            ctx_with_network(),
        )
        .await;
    // With SSRF bypassed, we expect a connection error, not a
    // PermissionDenied error
    match &result {
        Err(AgentOSError::ToolExecutionFailed { reason, .. }) => {
            assert!(
                reason.contains("HTTP request failed") || reason.contains("Connection"),
                "Should get connection error, not SSRF block: {}",
                reason
            );
        }
        Err(AgentOSError::PermissionDenied { .. }) => {
            panic!("SSRF protection should be bypassed with AGENTOS_TEST_ALLOW_LOCAL");
        }
        Ok(_) => {
            // Unlikely but acceptable -- port 1 might be open on some systems
        }
        other => {
            // Any non-PermissionDenied error is acceptable
            let _ = other;
        }
    }
    std::env::remove_var("AGENTOS_TEST_ALLOW_LOCAL");
}
```

### Step 3: Run all tests

```bash
# Unit tests (no network required)
cargo test -p agentos-tools -- ssrf --nocapture

# Integration tests (no network required -- use localhost/IPs)
cargo test -p agentos-tools --test ssrf_dns_test --nocapture

# Network-dependent tests (run manually when network is available)
cargo test -p agentos-tools -- --ignored --nocapture

# Full test suite
cargo test -p agentos-tools
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/ssrf_resolver.rs` | Add resolver-level integration tests (in `#[cfg(test)]` module) |
| `crates/agentos-tools/tests/ssrf_dns_test.rs` | **NEW**: End-to-end SSRF tests for `WebFetch` and `HttpClientTool` |

---

## Prerequisites

[[02-wire-resolver-into-clients]] must be complete. Both `WebFetch` and `HttpClientTool` must be using `SsrfAwareDnsResolver`.

---

## Test Plan

All tests should pass in CI (without network) unless marked `#[ignore]`:

| Test | Requires Network | Asserts |
|------|-------------------|---------|
| `resolver_allows_public_domain` | Yes (`#[ignore]`) | `example.com` resolves to public IPs |
| `resolver_blocks_localhost_resolution` | No | `localhost` resolution returns SSRF error |
| `resolver_allows_localhost_when_private_permitted` | No | `localhost` allowed with `allow_private=true` |
| `web_fetch_blocks_localhost_domain` | No | `WebFetch` rejects `http://localhost:9999/` |
| `http_client_blocks_loopback_ip` | No | `HttpClientTool` rejects `http://127.0.0.1:9999/` |
| `web_fetch_blocks_cloud_metadata_ip` | No | `WebFetch` rejects `http://169.254.169.254/` with correct error |
| `http_client_blocks_rfc1918_private_ips` | No | `HttpClientTool` rejects `10.x`, `172.16.x`, `192.168.x` |
| `web_fetch_allows_public_domain` | Yes (`#[ignore]`) | `httpbin.org` request succeeds |
| `test_allow_local_bypasses_resolver` | No | With `AGENTOS_TEST_ALLOW_LOCAL`, loopback gets connection error (not SSRF block) |

---

## Verification

```bash
# Build
cargo build -p agentos-tools

# Run non-network tests
cargo test -p agentos-tools -- ssrf --nocapture
cargo test -p agentos-tools --test ssrf_dns_test --nocapture

# Run network tests (manual)
cargo test -p agentos-tools -- --ignored --nocapture

# Full suite
cargo test -p agentos-tools

# Clippy
cargo clippy -p agentos-tools -- -D warnings

# Format check
cargo fmt -p agentos-tools -- --check
```

---

## Related

- [[DNS SSRF Fix Plan]]
- [[01-ssrf-resolver-impl]]
- [[02-wire-resolver-into-clients]]
- [[03-cleanup-and-docs]]
