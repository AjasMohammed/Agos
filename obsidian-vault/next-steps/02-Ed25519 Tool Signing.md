---
title: Ed25519 Tool Manifest Signing — Spec #1
tags:
  - next-steps
  - security
  - spec-1
date: 2026-03-11
status: done
effort: 6h
priority: high
spec-ref: "Spec §1 — Capability-Signed Skill Registry"
---

# Ed25519 Tool Manifest Signing

> Grounded in the OpenClaw/Cisco data exfiltration incident where a third-party skill performed exfiltration with no manifest integrity check.

---

## Current State

The `ToolManifest` struct (`crates/agentos-types/src/tool.rs`) has:
- `name`, `version`, `author`, `checksum` in `ToolInfo`
- No `signature`, no `author_pubkey`, no `trust_tier`

The `ToolRegistry` (`crates/agentos-kernel/src/tool_registry.rs`) accepts any manifest with no verification.

The `IdentityManager` (`crates/agentos-kernel/src/identity.rs`) already implements Ed25519 sign/verify — the primitive exists, it just isn't wired to tool loading.

---

## What Needs to Be Built

### Step 1 — Add `TrustTier` Enum to Types

**File:** `crates/agentos-types/src/tool.rs`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TrustTier {
    Core,       // Shipped with AgentOS, signed by foundation key
    Verified,   // Community, reviewed and co-signed by maintainers
    #[default]
    Community,  // Author-signed only; user must opt-in
    Blocked,    // Revoked; kernel hard-rejects even if locally installed
}
```

### Step 2 — Extend `ToolInfo` with Signing Fields

**File:** `crates/agentos-types/src/tool.rs`

```rust
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub checksum: Option<String>,
    // NEW fields:
    #[serde(default)]
    pub author_pubkey: Option<String>,    // Ed25519 pubkey (hex)
    #[serde(default)]
    pub signature: Option<String>,        // Ed25519 sig over canonical manifest bytes (hex)
    #[serde(default)]
    pub trust_tier: TrustTier,
}
```

> [!note] Backward Compatibility
> All new fields use `#[serde(default)]` — existing `tool.toml` files continue to parse, they just get `trust_tier: Community` and no signature.

### Step 3 — Canonical Signing Payload

Define what bytes are signed to prevent ambiguity. The signature covers a deterministic JSON serialization of:

```json
{
  "name": "<tool name>",
  "version": "<semver>",
  "description": "<description>",
  "author": "<author>",
  "capabilities": ["<cap1>", "<cap2>"],
  "sandbox": {
    "network": false,
    "fs_write": false,
    "max_memory_mb": 128,
    "max_cpu_ms": 5000
  }
}
```

**Helper to produce canonical bytes:**
```rust
// crates/agentos-types/src/tool.rs
impl ToolManifest {
    /// Returns the canonical bytes that are signed over.
    pub fn signing_payload(&self) -> Vec<u8> {
        let canonical = serde_json::json!({
            "name": self.manifest.name,
            "version": self.manifest.version,
            "description": self.manifest.description,
            "author": self.manifest.author,
            "capabilities": self.capabilities_required.permissions,
            "sandbox": {
                "network": self.sandbox.network,
                "fs_write": self.sandbox.fs_write,
                "max_memory_mb": self.sandbox.max_memory_mb,
                "max_cpu_ms": self.sandbox.max_cpu_ms,
            }
        });
        canonical.to_string().into_bytes()
    }
}
```

### Step 4 — Signature Verification in ToolRegistry

**File:** `crates/agentos-kernel/src/tool_registry.rs`

Modify `register()` to call a new `verify_tool_manifest()` function before accepting the tool:

```rust
// Add to ToolRegistry::register():
fn verify_tool_manifest(manifest: &ToolManifest) -> Result<(), AgentOSError> {
    use crate::identity::IdentityManager;

    // Blocked tools are hard-rejected regardless of signature
    if manifest.manifest.trust_tier == TrustTier::Blocked {
        return Err(AgentOSError::PermissionDenied(format!(
            "Tool '{}' is on the revocation list and cannot be loaded",
            manifest.manifest.name
        )));
    }

    // If a signature is present, verify it
    if let (Some(pubkey), Some(sig)) = (
        &manifest.manifest.author_pubkey,
        &manifest.manifest.signature,
    ) {
        let payload = manifest.signing_payload();
        let valid = IdentityManager::verify_signature(pubkey, &payload, sig)
            .map_err(|e| AgentOSError::Internal(format!("Signature check error: {e}")))?;

        if !valid {
            return Err(AgentOSError::PermissionDenied(format!(
                "Tool '{}' has an invalid Ed25519 signature — refusing to load",
                manifest.manifest.name
            )));
        }
    } else if manifest.manifest.trust_tier == TrustTier::Core
        || manifest.manifest.trust_tier == TrustTier::Verified
    {
        // Core and Verified tools MUST be signed
        return Err(AgentOSError::PermissionDenied(format!(
            "Tool '{}' claims trust tier {:?} but has no signature",
            manifest.manifest.name, manifest.manifest.trust_tier
        )));
    }

    Ok(())
}
```

### Step 5 — CLI: `agentctl tool sign`

**New subcommand in `crates/agentos-cli/src/commands/tool.rs`:**

```rust
// ToolCommands::Sign { manifest_path, key_path }
// 1. Load tool.toml from manifest_path
// 2. Load agent Ed25519 private key from key_path (or from vault)
// 3. Compute signing_payload()
// 4. Sign with Ed25519
// 5. Write signature + pubkey back into tool.toml
```

**Usage:**
```bash
agentctl tool sign --manifest ./tools/browser/tool.toml --key ~/.agentOS/author.key
agentctl tool verify --manifest ./tools/browser/tool.toml
```

### Step 6 — Add `TrustTier` to re-exports

**File:** `crates/agentos-types/src/lib.rs`

```rust
pub use tool::{
    ExecutorType, RegisteredTool, ToolExecutor, ToolManifest,
    ToolSandbox, ToolStatus, TrustTier,  // ← add TrustTier
};
```

---

## Testing Plan

| Test | Location | Verifies |
|---|---|---|
| `test_unsigned_community_tool_loads` | `tool_registry.rs` | Unsigned Community tool is accepted |
| `test_valid_signature_verified_tool_loads` | `tool_registry.rs` | Verified tool with valid sig loads |
| `test_invalid_signature_rejected` | `tool_registry.rs` | Bad sig on Verified tool is rejected |
| `test_blocked_tool_hard_rejected` | `tool_registry.rs` | Blocked tier is always rejected |
| `test_core_tool_without_sig_rejected` | `tool_registry.rs` | Core tier without sig is rejected |
| `test_signing_payload_deterministic` | `types/tool.rs` | Same manifest → same payload bytes |

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-types/src/tool.rs` | Add `TrustTier` enum, add fields to `ToolInfo`, add `signing_payload()` method |
| `crates/agentos-types/src/lib.rs` | Re-export `TrustTier` |
| `crates/agentos-kernel/src/tool_registry.rs` | Add `verify_tool_manifest()`, call in `register()` |
| `crates/agentos-cli/src/commands/tool.rs` | Add `Sign` and `Verify` subcommands |
| `crates/agentos-bus/src/message.rs` | Add `KernelCommand::VerifyToolManifest { path }` if needed |

---

## Related

- [[Index]] — Back to dashboard
- [[reference/Tool System]] — Existing tool system documentation
- [[reference/Security Model]] — Security model overview
