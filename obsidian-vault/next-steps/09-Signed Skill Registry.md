---
title: Signed Skill Registry
tags:
  - security
  - kernel
  - phase-0
  - feature
date: 2026-03-11
status: in-progress
effort: 1d
priority: critical
---

# Signed Skill Registry

> Implement Ed25519 manifest signing and trust-tier enforcement so the kernel hard-rejects unsigned, revoked, or tampered tools — closing the exfiltration vector confirmed in OpenClaw/Cisco.

---

## Current State

- `ToolInfo` has only a `checksum: Option<String>` field — no `author_pubkey`, `signature`, or `trust_tier`
- `ToolRegistry::register()` accepts any parsed manifest with no verification
- `cmd_install_tool` parses TOML and registers immediately — no trust check
- Core tool manifests (`tools/core/*.toml`) have no trust tier annotation
- Ed25519 infrastructure exists in `identity.rs` (kernel crate) but is not wired to tool loading

## Goal / Target State

- `ToolManifest` carries `trust_tier`, `author_pubkey`, and `signature` fields
- Kernel hard-rejects `TrustTier::Blocked` tools at registration
- `Community` and `Verified` tools require a valid Ed25519 signature over a canonical signing payload
- `Core` tools (shipped with AgentOS) are distribution-trusted — no runtime signature needed
- CLI provides offline `tool sign`, `tool verify`, and `tool keygen` commands for tool authors
- All seven core manifests annotated with `trust_tier = "core"`

## Step-by-Step Plan

1. Write obsidian planning doc (this file) ✅
2. Add `TrustTier` enum and signature fields to `agentos-types/src/tool.rs`; export from crate root
3. Add `ToolBlocked` and `ToolSignatureInvalid` error variants to `AgentOSError`
4. Add `ed25519-dalek` + `hex` deps to `agentos-tools/Cargo.toml`
5. Create `agentos-tools/src/signing.rs` — `signing_payload()` + `verify_manifest_signature()`
6. Update `agentos-tools/src/loader.rs` — call `verify_manifest_signature` after TOML parse
7. Update `agentos-kernel/src/tool_registry.rs` — `register()` enforces trust tier policy
8. Update `agentos-kernel/src/commands/tool.rs` — `cmd_install_tool` propagates errors
9. Add `Sign`, `Verify`, `Keygen` subcommands to `agentos-cli/src/commands/tool.rs` (offline, no bus)
10. Add `trust_tier = "core"` to all seven `tools/core/*.toml` manifests
11. `cargo test --workspace` — all tests pass

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/tool.rs` | Add `TrustTier`, extend `ToolInfo` |
| `crates/agentos-types/src/lib.rs` | Export `TrustTier` |
| `crates/agentos-types/src/error.rs` | Add `ToolBlocked`, `ToolSignatureInvalid` |
| `crates/agentos-tools/Cargo.toml` | Add `ed25519-dalek`, `hex` deps |
| `crates/agentos-tools/src/signing.rs` | NEW — signing payload + Ed25519 verification |
| `crates/agentos-tools/src/loader.rs` | Call signature verification on load |
| `crates/agentos-tools/src/lib.rs` | Export `signing` module |
| `crates/agentos-kernel/src/tool_registry.rs` | Trust tier enforcement in `register()` |
| `crates/agentos-kernel/src/commands/tool.rs` | Propagate errors from registry |
| `crates/agentos-cli/src/commands/tool.rs` | Add `Sign`, `Verify`, `Keygen` subcommands |
| `tools/core/*.toml` (7 files) | Add `trust_tier = "core"` |

## Trust Tier Policy

| Tier | Signature Required | Who Signs | Kernel Behavior |
|------|--------------------|-----------|-----------------|
| `core` | No (distribution-trusted) | AgentOS foundation | Accept unconditionally |
| `verified` | Yes — author + maintainer | Author + co-sign | Verify author_pubkey signature |
| `community` | Yes — author only | Author | Verify author_pubkey signature |
| `blocked` | N/A | N/A | Hard reject, log to audit |

## Signing Payload (Canonical JSON)

Signed fields (sorted keys, no whitespace):
```json
{"author":"...","capabilities":[...],"max_cpu_ms":...,"max_memory_mb":...,"name":"...","network":...,"version":"..."}
```

Description and checksum are excluded (mutable metadata). Signature is excluded (circular).

## Verification

```bash
# Install a community tool without signature → should fail
agentctl tool install ./tools/example/unsigned.toml
# Expected: Error: tool 'example' has trust_tier=community but missing signature

# Install a blocked tool → should hard-reject
agentctl tool install ./tools/example/blocked.toml
# Expected: Error: tool 'revoked-tool' is blocked and cannot be loaded

# Sign a manifest (offline)
agentctl tool keygen --output ./my-keypair.json
agentctl tool sign --manifest ./tool.toml --key ./my-keypair.json
agentctl tool verify --manifest ./tool.toml

# Core tools load without signature
cargo test -p agentos-tools -- signing
cargo test --workspace
```

## Related

- [[02-Ed25519 Tool Signing]]
- [[Tool System]]
- [[Capability and Permissions]]
