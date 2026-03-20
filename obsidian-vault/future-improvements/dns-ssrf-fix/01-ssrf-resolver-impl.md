---
title: Implement SsrfAwareDnsResolver
tags:
  - security
  - tools
  - v3
  - next-steps
date: 2026-03-19
status: planned
effort: 4h
priority: high
---

# Implement SsrfAwareDnsResolver

> Create a new `ssrf_resolver.rs` module in `agentos-tools` that implements `reqwest::dns::Resolve` using `hickory-resolver`, validating all resolved IP addresses against private/loopback/link-local ranges before returning them to reqwest.

---

## Why This Phase

This is the foundation phase. The `SsrfAwareDnsResolver` is the core component that closes the DNS rebinding SSRF gap. Without it, reqwest resolves DNS internally and connects to whatever IP the DNS server returns -- including private IPs that bypass all hostname-based SSRF checks. This phase builds the resolver and its unit tests in isolation, without touching the existing tool code.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| DNS resolution | Handled internally by reqwest; no IP validation | Custom resolver validates every resolved IP |
| `is_private_ip()` | Duplicated in `web_fetch.rs` and `http_client.rs` | Consolidated into `ssrf_resolver.rs` as `pub(crate) fn is_private_ip()` |
| `hickory-resolver` dependency | Not in workspace | Added to `agentos-tools/Cargo.toml` |
| `ssrf_resolver` module | Does not exist | New module: `crates/agentos-tools/src/ssrf_resolver.rs` |
| SSRF IP ranges blocked | Loopback, private (RFC1918), link-local, multicast, unspecified | Same ranges + explicit cloud metadata logging |

---

## What to Do

### Step 1: Add `hickory-resolver` dependency

Open `crates/agentos-tools/Cargo.toml` and add to `[dependencies]`:

```toml
hickory-resolver = { version = "0.24", features = ["tokio-runtime"] }
```

This brings in the async DNS resolver with tokio integration.

### Step 2: Create `crates/agentos-tools/src/ssrf_resolver.rs`

Create a new file with the following structure:

```rust
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use hickory_resolver::TokioAsyncResolver;
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tracing::warn;

/// DNS resolver that blocks resolution to private/loopback/link-local IPs.
///
/// Implements `reqwest::dns::Resolve` so it can be injected into any
/// `reqwest::Client` via `ClientBuilder::dns_resolver()`. This ensures
/// that every DNS resolution -- including the initial request and all
/// redirect hops -- is validated against SSRF-unsafe IP ranges.
///
/// # TOCTOU Safety
///
/// Because reqwest uses this resolver for the actual connection (not a
/// separate pre-flight lookup), there is no time-of-check/time-of-use
/// gap. The IPs returned by this resolver are the IPs reqwest connects to.
pub struct SsrfAwareDnsResolver {
    resolver: TokioAsyncResolver,
    allow_private: bool,
}

impl SsrfAwareDnsResolver {
    /// Create a new resolver using system DNS configuration.
    ///
    /// # Arguments
    /// * `allow_private` - If `true`, skip IP validation (test-only).
    ///   Logs a warning when set.
    pub fn new(allow_private: bool) -> Self {
        if allow_private {
            warn!(
                "SsrfAwareDnsResolver: private IP blocking DISABLED \
                 (AGENTOS_TEST_ALLOW_LOCAL) -- do not use in production"
            );
        }
        let resolver = TokioAsyncResolver::tokio(
            ResolverConfig::default(),
            ResolverOpts::default(),
        );
        Self {
            resolver,
            allow_private,
        }
    }
}

impl Resolve for SsrfAwareDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.resolver.clone();
        let allow_private = self.allow_private;
        let hostname = name.as_str().to_string();

        Box::pin(async move {
            let lookup = resolver
                .lookup_ip(hostname.as_str())
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    Box::new(io::Error::new(
                        io::ErrorKind::Other,
                        format!("DNS resolution failed for {}: {}", hostname, e),
                    ))
                })?;

            let ips: Vec<IpAddr> = lookup.iter().collect();

            if ips.is_empty() {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::Other,
                    format!("DNS resolution returned no addresses for {}", hostname),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }

            if !allow_private {
                for ip in &ips {
                    if is_ssrf_unsafe(ip) {
                        warn!(
                            hostname = %hostname,
                            resolved_ip = %ip,
                            "SSRF: DNS resolution to private/loopback IP blocked"
                        );
                        return Err(Box::new(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!(
                                "SSRF protection: {} resolved to private/loopback IP {}",
                                hostname, ip
                            ),
                        )) as Box<dyn std::error::Error + Send + Sync>);
                    }
                }
            }

            // Return all validated IPs on port 0 -- reqwest overrides
            // the port with the URL's port or the scheme default.
            let addrs: Addrs = Box::new(
                ips.into_iter()
                    .map(|ip| SocketAddr::new(ip, 0)),
            );
            Ok(addrs)
        })
    }
}

/// Returns `true` if the IP address is in a range that should be
/// blocked for outbound SSRF protection.
///
/// Blocked ranges:
/// - IPv4 loopback: `127.0.0.0/8`
/// - IPv4 private: `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`
/// - IPv4 link-local: `169.254.0.0/16` (includes cloud metadata `169.254.169.254`)
/// - IPv4 unspecified: `0.0.0.0`
/// - IPv4 multicast: `224.0.0.0/4`
/// - IPv6 loopback: `::1`
/// - IPv6 unspecified: `::`
/// - IPv6 multicast: `ff00::/8`
/// - IPv6 unique local: `fc00::/7`
/// - IPv6 link-local: `fe80::/10`
pub(crate) fn is_ssrf_unsafe(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            ipv4.is_loopback()
                || ipv4.is_private()
                || ipv4.is_link_local()
                || ipv4.is_unspecified()
                || ipv4.is_multicast()
        }
        IpAddr::V6(ipv6) => {
            ipv6.is_loopback()
                || ipv6.is_unspecified()
                || ipv6.is_multicast()
                // fc00::/7 -- unique local
                || (ipv6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10 -- link local
                || (ipv6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Convenience alias for the old function name used in `web_fetch.rs`
/// and `http_client.rs`. Both modules should migrate to importing
/// `is_ssrf_unsafe` from this module.
pub(crate) fn is_private_ip(ip: &IpAddr) -> bool {
    is_ssrf_unsafe(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- is_ssrf_unsafe unit tests --

    #[test]
    fn blocks_ipv4_loopback() {
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(127, 255, 255, 255))));
    }

    #[test]
    fn blocks_ipv4_private_rfc1918() {
        // 10.0.0.0/8
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        // 172.16.0.0/12
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        // 192.168.0.0/16
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
    }

    #[test]
    fn blocks_cloud_metadata_ip() {
        // AWS/GCP/Azure metadata endpoint
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
    }

    #[test]
    fn blocks_ipv4_link_local() {
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1))));
    }

    #[test]
    fn blocks_ipv4_unspecified() {
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
    }

    #[test]
    fn blocks_ipv4_multicast() {
        assert!(is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
    }

    #[test]
    fn allows_public_ipv4() {
        assert!(!is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_ssrf_unsafe(&IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))));
    }

    #[test]
    fn blocks_ipv6_loopback() {
        assert!(is_ssrf_unsafe(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn blocks_ipv6_unspecified() {
        assert!(is_ssrf_unsafe(&IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
    }

    #[test]
    fn blocks_ipv6_unique_local() {
        // fc00::/7
        let ip: Ipv6Addr = "fd00::1".parse().unwrap();
        assert!(is_ssrf_unsafe(&IpAddr::V6(ip)));
    }

    #[test]
    fn blocks_ipv6_link_local() {
        // fe80::/10
        let ip: Ipv6Addr = "fe80::1".parse().unwrap();
        assert!(is_ssrf_unsafe(&IpAddr::V6(ip)));
    }

    #[test]
    fn allows_public_ipv6() {
        let ip: Ipv6Addr = "2606:4700:4700::1111".parse().unwrap();
        assert!(!is_ssrf_unsafe(&IpAddr::V6(ip)));
    }

    // -- SsrfAwareDnsResolver integration tests --
    // These require network access and are tested in Phase 04.
}
```

### Step 3: Register the module in `lib.rs`

Open `crates/agentos-tools/src/lib.rs` and add the module declaration. Place it alphabetically near the other module declarations:

```rust
pub mod ssrf_resolver;
```

No public re-export is needed at the crate root since `is_ssrf_unsafe` and the resolver are `pub(crate)`.

### Step 4: Verify compilation

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- ssrf_resolver --nocapture
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/Cargo.toml` | Add `hickory-resolver = { version = "0.24", features = ["tokio-runtime"] }` |
| `crates/agentos-tools/src/ssrf_resolver.rs` | **NEW**: `SsrfAwareDnsResolver` struct, `is_ssrf_unsafe()`, `is_private_ip()` alias, unit tests |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod ssrf_resolver;` |

---

## Prerequisites

None -- this is the first phase.

---

## Test Plan

- `cargo test -p agentos-tools -- ssrf_resolver` must pass
- Unit tests assert `is_ssrf_unsafe` returns `true` for:
  - `127.0.0.1` (loopback)
  - `10.0.0.1`, `172.16.0.1`, `192.168.1.1` (RFC1918 private)
  - `169.254.169.254` (cloud metadata / link-local)
  - `0.0.0.0` (unspecified)
  - `224.0.0.1` (multicast)
  - `::1` (IPv6 loopback)
  - `fd00::1` (IPv6 unique local)
  - `fe80::1` (IPv6 link-local)
- Unit tests assert `is_ssrf_unsafe` returns `false` for:
  - `8.8.8.8` (Google DNS)
  - `1.1.1.1` (Cloudflare DNS)
  - `2606:4700:4700::1111` (Cloudflare IPv6)
- `is_private_ip` alias returns same result as `is_ssrf_unsafe` for all inputs

---

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- ssrf_resolver --nocapture
cargo clippy -p agentos-tools -- -D warnings
```

---

## Related

- [[DNS SSRF Fix Plan]]
- [[02-wire-resolver-into-clients]]
