---
title: Gmail MCP Server — Research Synthesis
tags:
  - mcp
  - gmail
  - research
  - security
date: 2026-04-18
status: complete
effort: 1d
priority: high
---

# Gmail MCP Server — Research Synthesis

> Why we're building a standalone, production-grade Gmail MCP server, and how we settled on Rust + keychain + MCP-compliant stdio.

---

## Problem statement

MCP-aware hosts (AgentOS, Claude Desktop, Cursor, OpenClaw, Cline) need Gmail access, but the existing server implementations are inadequate for any deployment beyond a personal laptop experiment.

---

## Audit of existing Gmail MCP servers

See [[MCP Catalog Installer Research]] for the full audit rubric. Summary:

| Server | Stars | Maint. | Token store | Scopes | Tests/CI | Verdict |
|--------|-------|--------|-------------|--------|----------|---------|
| `@gongrzhe/server-gmail-autoauth-mcp` | 1,091 | **Archived** | Plaintext JSON | Hardcoded modify | No / No | **Rejected** |
| `@shinzolabs/gmail-mcp` | 51 | Active | Plaintext JSON | Fixed | No / No | Not production |
| `gmail-app-password-mcp` | ~10 | Dormant | App password | N/A | No / No | Fundamental security issues |
| `taylorwilsdon/google_workspace_mcp` | 2,139 | **Active** | File-based | Configurable | Partial / Yes | **Best OSS alternative, Python only** |
| Anthropic hosted Gmail | N/A | Active | Managed by Anthropic | Managed | N/A | Claude-only; not portable |

**Nothing in this list meets the bar.** Every self-hostable option writes OAuth refresh tokens to a plaintext file by default, and none offer the single-static-binary / keychain story enterprises expect.

---

## Language choice — why Rust

Considered: Rust, Go, Node.js (TypeScript), Python.

| Dimension | Rust | Go | Node | Python |
|-----------|------|----|----|--------|
| Single static binary | ✅ | ✅ | ❌ (npm) | ❌ (pip) |
| Cold start | <50 ms | <50 ms | 200–2000 ms | 300–1500 ms |
| Memory safety | ✅ | ✅ (GC) | JS | Python |
| Keychain libs | `keyring` (mature) | `zalando/go-keyring` | `keytar` (native) | `keyring` (mature) |
| MCP SDK quality | Ad-hoc (we roll our own) | Ad-hoc | Official but churning | Official (rmcp) |
| Supply-chain audit ease | Small (<50 crates) | Small | Large (`node_modules`) | Medium |
| AgentOS affinity | Native | None | None | None |
| Gmail API client crate | `google-gmail1` (good) | `google-api-go-client` (good) | `googleapis` (reference) | `google-api-python-client` (reference) |

Rust wins on the dimensions that matter for a production security-sensitive daemon. Go is a very close second but offers nothing that Rust doesn't, and we already have Rust expertise in this ecosystem.

We accept the "roll our own MCP layer" cost. It's ~300 lines and lets us stay current with the spec.

---

## Why standalone vs. fork

Considered: fork `taylorwilsdon/google_workspace_mcp` vs. greenfield.

**Fork pros:**
- ~3-4 days less work.
- Immediate feature parity (Gmail + Calendar + Drive + Docs).

**Fork cons:**
- Inherits Python runtime dependency.
- Larger scope = larger audit surface. Gmail-only is tractable; Workspace-wide isn't for a security-critical daemon.
- Token storage is still file-based; to fix it properly we'd have to rewrite core modules.
- Upstream change ownership ambiguity.

**Greenfield pros:**
- Gmail-only scope is auditable in a week by one engineer.
- Rust static binary from day one.
- Design token storage right the first time.
- Own the release cadence and security disclosure process.
- Can be packaged for AgentOS catalog with zero compromises.

**Greenfield cons:**
- Longer build (we're budgeting ~14 days vs 3-4).
- Initial feature set smaller; no Calendar/Drive/Docs at launch.

**Decision:** greenfield. We keep the scope narrow (Gmail only) and ship Calendar/Drive/Docs as sibling binaries later if demand appears.

---

## MCP compatibility is automatic

MCP is a protocol, not a framework. The spec ([modelcontextprotocol.io](https://modelcontextprotocol.io)) defines JSON-RPC 2.0 messages over stdio or HTTP/SSE:

- `initialize` — handshake exchanging `protocolVersion` and `capabilities`.
- `tools/list` — enumerate available tools with JSON Schema input/output.
- `tools/call` — invoke a tool with JSON input, receive JSON output.
- Plus `prompts/*`, `resources/*`, notifications — optional.

Any client that implements the protocol can spawn any server that implements the protocol. Claude Desktop, Cursor, Cline, Continue, OpenClaw, AgentOS — all of them speak the same protocol. We build a compliant server once, and it runs everywhere.

We do not need per-client adaptation.

---

## OAuth flow options

Gmail's OAuth 2.0 supports several grant flows. We need to pick what to implement.

| Flow | When appropriate | Required |
|------|------------------|----------|
| Desktop app (loopback) | User has a GUI browser | ✅ v1 default |
| Device code | Headless hosts (SSH, CI) | ✅ v1 (stretch in Phase 3) |
| Service account | Google Workspace admin with domain-wide delegation | ✅ v1 |
| Web app (redirect) | A hosted service | ❌ Not applicable to a CLI |
| Installed client w/ OOB code | **Deprecated by Google** | ❌ Never |

**v1 ships all three: desktop loopback as default, device code for headless, service account for Workspace.**

Notes:
- Loopback flow requires a local HTTP listener on a random port, redirect URI `http://127.0.0.1:<port>/`. Google explicitly supports this for installed apps.
- Device code flow surfaces an 8-digit code + verification URL; user completes on another device. Slower but required for headless VMs.
- Service account requires domain admin to delegate; only viable for Google Workspace tenants. Implementation is a ~50-line alternate auth module.

---

## Token storage options

We evaluated:

1. **Plaintext JSON file** — what every current MCP Gmail server does. Unacceptable.
2. **Encrypted JSON file (AES-256-GCM, key from Argon2id of user passphrase)** — works everywhere; requires passphrase prompt on unlock; acceptable fallback.
3. **OS keychain** — best. macOS Keychain, Windows Credential Manager, Linux Secret Service (GNOME Keyring, KWallet).
4. **Kernel keyring (Linux only)** — nichy; not portable.
5. **HSM / TPM-bound** — overkill for v1; possibly v2.

**v1 ships: keychain default, encrypted file fallback.** The `keyring` crate provides a unified API across macOS/Windows/Linux. Fallback activates when Secret Service is unavailable (headless Linux). Plaintext mode is gated behind `--token-store file-plaintext` with a huge warning and a command that refuses to enable it in "production" mode.

---

## Rate limiting approach

Gmail API quotas:
- 1,000,000,000 quota units per day per project (shared across users).
- 250 quota units per user per second.
- Per-tool costs vary (a `send` is 100 units; a `get` is 5).

Strategy:
- Token-bucket limiter at the HTTP client layer, 200 units/sec/user headroom (leaves slack for background refreshes).
- On 429 response: exponential backoff with jitter (1s, 2s, 4s, 8s, cap 30s, 5 retries).
- On 403 quota exhaustion: don't retry, return typed error immediately.
- Tool authors declare unit cost on each tool; limiter deducts before the call.

---

## Error model

Typed errors that clients can handle, not stringly-typed exceptions:

```rust
pub enum GmailMcpError {
    AuthExpired,                  // prompt user to re-auth
    AuthRevoked,                  // same, plus audit event
    ScopeMissing { required: Vec<String> },
    RateLimited { retry_after: Duration },
    QuotaExhausted,
    MessageNotFound { id: String },
    ThreadNotFound { id: String },
    LabelNotFound { name: String },
    InvalidQuery { reason: String },
    AttachmentTooLarge { size: u64, limit: u64 },
    TransportError(String),
    Internal(String),              // bug — please file an issue
}
```

Each variant maps to a distinct JSON-RPC error code. Clients can render remediation UI.

---

## Distribution strategy

Commercial Rust CLIs (ripgrep, fd, bat, uv, sigstore, atuin) all follow the same playbook:

1. **GitHub Releases** with signed tarballs per target triple.
2. **cosign** / sigstore for transparent signing + SLSA provenance.
3. **Homebrew tap** (`brew install <org>/tap/gmail-mcp`).
4. **Docker image** published to GHCR (`ghcr.io/<org>/gmail-mcp`).
5. **`cargo install gmail-mcp`** for Rust-native users.
6. **Scoop / winget** for Windows (stretch).
7. **AUR + apt PPA** community-maintained (stretch; accept PRs).

We adopt this playbook unchanged.

---

## Related

- [[Gmail MCP Server Plan]]
- [[Gmail MCP Server Data Flow]]
- [[MCP Catalog Installer Research]]
