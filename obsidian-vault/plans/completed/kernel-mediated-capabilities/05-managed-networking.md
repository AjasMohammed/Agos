---
title: "Phase 5: Managed Networking"
tags:
  - kernel
  - capabilities
  - security
  - v4
  - phase-5
date: 2026-04-12
status: planned
effort: 2d
priority: high
---

# Phase 5: Managed Networking (`net.*`)

> Replace binary network on/off with per-destination allowlists, rate limiting, and kernel-proxied requests — so agents can talk to APIs and databases without unrestricted network access.

---

## Why This Phase

AgentOS currently has two network modes: completely off (default for sandboxed tools) or completely on (shell-exec with `allow_network=true`). There's no middle ground. An agent that needs to call `api.github.com` gets either no network or full network — including the ability to hit `169.254.169.254` (cloud metadata), `localhost:5432` (databases), or exfiltrate data to arbitrary endpoints.

The `http_client` and `web_fetch` tools exist but use the host network stack directly. SSRF checks in `PermissionSet` block private IP ranges but there's no per-destination policy.

Managed networking proxies all agent network requests through the kernel, applying destination allowlists, rate limits, and audit logging per request.

---

## Current State

- Shell-exec: binary `--share-net` flag in bwrap (`shell_exec.rs`)
- `http_client` tool: direct HTTP via reqwest, SSRF check on private IPs (`capability.rs:73-101`)
- `web_fetch` tool: fetches web content, returns text
- `web_search` tool: 4-provider fallback with SSRF guard on DuckDuckGo
- `PermissionSet` has `deny_entries` but no per-destination network policy
- No rate limiting per destination

## Target State

- `NetworkProvider` implements `CapabilityProvider` for domain `"net"`
- Per-destination allowlist in config (glob patterns on host:port)
- Rate limiting per agent per destination
- All network requests proxied through kernel, audited
- Unknown destinations trigger escalation
- Existing `http_client`/`web_fetch` tools refactored to use `NetworkProvider`

---

## Detailed Subtasks

### 1. Define network policy model

**File:** `crates/agentos-kernel/src/managed_network.rs` (new)

```rust
use serde::{Deserialize, Serialize};

/// Network destination allowlist entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkDestination {
    /// Host pattern (glob): "*.github.com", "api.openai.com", "localhost"
    pub host_pattern: String,
    /// Port (None = any port)
    pub port: Option<u16>,
    /// Allowed HTTP methods (None = all)
    pub methods: Option<Vec<String>>,
    /// Rate limit: requests per minute (None = unlimited)
    pub rate_limit_rpm: Option<u32>,
}

/// Per-agent network session tracking.
pub struct AgentNetworkSession {
    pub agent_id: AgentID,
    /// Destination -> request count in current window
    pub request_counts: HashMap<String, RateWindow>,
    /// Session-specific grants (from dynamic negotiation)
    pub session_grants: Vec<NetworkDestination>,
}

pub struct RateWindow {
    pub count: u32,
    pub window_start: chrono::DateTime<chrono::Utc>,
}
```

### 2. Network policy configuration

**File:** `config/default.toml` (add section)

```toml
[capabilities.net]
# Default destination allowlist — agents can access these without approval.
# Format: "host_pattern[:port]"
allowed_destinations = [
    "*.github.com",
    "*.githubusercontent.com",
    "api.openai.com",
    "api.anthropic.com",
    "api.cohere.com",
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "*.googleapis.com",
]

# Destinations that are NEVER accessible (deny > allow).
denied_destinations = [
    "169.254.169.254",          # Cloud metadata
    "metadata.google.internal", # GCP metadata
    "10.*",                     # Private class A
    "172.16.*",                 # Private class B
    "192.168.*",                # Private class C
    "127.*",                    # Loopback (use explicit localhost grant)
    "0.0.0.0",
]

# Default rate limit per agent per destination (requests/minute)
default_rate_limit_rpm = 60

# Maximum response body size (bytes)
max_response_body_bytes = 10_485_760  # 10 MB

# Request timeout (seconds)
request_timeout_secs = 30
```

### 3. Implement `NetworkProvider`

Actions:
- **`http`** — Make an HTTP request:
  1. Parse URL → extract host:port
  2. Check denied destinations (deny > allow)
  3. Check allowed destinations (glob match on host)
  4. If not allowed: create `PendingEscalation` or return error
  5. Check rate limit for agent+destination
  6. Execute request via shared `reqwest::Client`
  7. Enforce response body size limit
  8. Audit: `NetworkRequestExecuted` with destination, method, status code, body size
  9. Return structured response (status, headers, body)

- **`connect`** — TCP connection to approved host:port:
  1. Same destination validation as HTTP
  2. Return connection handle (for database connections, etc.)
  3. This is more complex — initially return "not yet implemented" and focus on HTTP

- **`dns`** — DNS resolution:
  1. Resolve hostname, return IPs
  2. Check resolved IPs against denied list (SSRF via DNS rebinding defense)
  3. Audit: DNS resolution logged

### 4. Refactor existing network tools

**Files:** `crates/agentos-tools/src/http_client.rs`, `web_fetch.rs`, `web_search.rs`

Refactor these to delegate to `NetworkProvider` instead of making direct HTTP requests:
- Extract destination from URL
- Call `NetworkProvider.execute("http", ...)` 
- Use the result

This ensures ALL network traffic flows through the same policy engine.

### 5. Convenience tools and manifests

- `net-http` — `{ "url": "https://api.github.com/repos/...", "method": "GET", "headers": {} }`
- `net-dns` — `{ "hostname": "api.github.com" }`

Manifests with `risk_class = "readonly_external"`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/managed_network.rs` | NEW — `NetworkProvider`, destination policy, rate limiting |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod managed_network;` |
| `crates/agentos-kernel/src/kernel.rs` | Register `NetworkProvider` at boot |
| `crates/agentos-tools/src/http_client.rs` | Refactor to use `NetworkProvider` |
| `crates/agentos-tools/src/web_fetch.rs` | Refactor to use `NetworkProvider` |
| `crates/agentos-tools/src/web_search.rs` | Refactor to use `NetworkProvider` |
| `crates/agentos-tools/src/net_tools.rs` | NEW — convenience tools |
| `crates/agentos-tools/src/factory.rs` | Register net tools |
| `config/default.toml` | Add `[capabilities.net]` section |
| `tools/core/net-*.toml` | NEW — 2 manifests |

---

## Dependencies

- **Requires:** Phase 1 (capability provider trait)
- **Blocks:** Nothing directly

---

## Test Plan

- [ ] HTTP request to allowed destination succeeds
- [ ] HTTP request to denied destination returns error
- [ ] HTTP request to unknown destination creates escalation
- [ ] DNS rebinding defense: hostname resolving to private IP blocked
- [ ] Rate limit enforced: Nth+1 request within window returns rate limit error
- [ ] Response body size limit enforced (truncated at max)
- [ ] Request timeout enforced
- [ ] Existing `http_client` tool routes through `NetworkProvider`
- [ ] Existing `web_fetch` tool routes through `NetworkProvider`
- [ ] Per-agent rate tracking — agent A's requests don't count against agent B
- [ ] Session grants from dynamic negotiation (Phase 7) work
- [ ] Audit events: `NetworkRequestExecuted`, `NetworkDestinationBlocked`

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- managed_network
cargo test -p agentos-tools -- net_tools
cargo test -p agentos-tools -- http_client
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Related

- [[01-capability-provider-trait]] — prerequisite
- [[07-dynamic-capability-negotiation]] — session grants for new destinations
- [[Kernel Mediated Capabilities Plan]]
