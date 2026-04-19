---
title: "Phase 8: Policy Engine & Operator Controls"
tags:
  - kernel
  - capabilities
  - security
  - cli
  - v4
  - phase-8
date: 2026-04-12
status: planned
effort: 2d
priority: medium
---

# Phase 8: Policy Engine & Operator Controls

> Unified policy engine for all capability domains, CLI commands for operators to manage policies and review grants, and default policy profiles for common use cases.

---

## Why This Phase

Phases 2-7 each introduce per-domain policy configuration. Without a unified engine, operators must configure allowlists, rate limits, and approval rules separately in each `[capabilities.*]` config section. The policy engine:

1. Provides a consistent rule evaluation model across all domains
2. Offers CLI commands to inspect, modify, and test policies live
3. Ships default policy profiles ("development", "production", "restricted") to reduce setup burden
4. Adds a dashboard view of active grants, pending escalations, and policy matches

This is the operator-facing surface that makes KMC practical in production. Without it, the system works but is hard to administer.

---

## Current State

- Per-domain config sections: `[capabilities.env]`, `[capabilities.net]`, etc. (from Phases 2-5)
- `CapabilityBroker` evaluates policy per-request (from Phase 7)
- `ConfigWatcher` reloads config on file change (from OpenClaw Phase 2)
- CLI commands exist for agent, task, tool management — no capability management
- Web UI has agents and tasks pages — no capability/policy pages

## Target State

- `PolicyEngine` — unified rule evaluation across all domains
- Policy profiles: "development" (broad), "production" (curated), "restricted" (minimal)
- CLI: `agentos capability list/grants/revoke/policy`
- Web UI: capability grants dashboard, policy editor (future phase)
- Policy reload via `ConfigWatcher` without kernel restart

---

## Detailed Subtasks

### 1. Unified policy engine

**File:** `crates/agentos-kernel/src/policy_engine.rs` (new)

```rust
use serde::{Deserialize, Serialize};

/// A policy rule that applies to one or more capability domains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Rule ID for reference in audit logs
    pub id: String,
    /// Which domains this rule applies to ("*" = all)
    pub domains: Vec<String>,
    /// Which actions this rule applies to ("*" = all)
    pub actions: Vec<String>,
    /// Resource pattern (glob)
    pub resource_pattern: String,
    /// What to do when this rule matches
    pub effect: PolicyEffect,
    /// Priority (higher number = checked first)
    pub priority: u32,
    /// Optional: only applies to agents with this trust tier or higher
    pub min_trust_tier: Option<TrustTier>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PolicyEffect {
    /// Auto-grant without human approval
    Allow,
    /// Always deny (no escalation, no override)
    Deny,
    /// Require human approval via escalation
    Escalate,
    /// Allow with rate limiting
    RateLimit { requests_per_minute: u32 },
}

pub struct PolicyEngine {
    rules: Vec<PolicyRule>,
}

impl PolicyEngine {
    /// Evaluate a capability request against all rules.
    /// Returns the effect of the highest-priority matching rule.
    /// If no rule matches, defaults to Escalate (fail-safe).
    pub fn evaluate(
        &self,
        domain: &str,
        action: &str,
        resource: &str,
        agent_trust_tier: Option<&TrustTier>,
    ) -> PolicyEffect;

    /// Reload rules from config file.
    pub fn reload_from_config(&mut self, config: &toml::Value) -> Result<(), AgentOSError>;

    /// List all rules (for CLI display).
    pub fn rules(&self) -> &[PolicyRule];

    /// Test a hypothetical request against policy (dry-run, for operators).
    pub fn dry_run(
        &self,
        domain: &str,
        action: &str,
        resource: &str,
    ) -> PolicyEvaluationResult;
}
```

### 2. Default policy profiles

**File:** `config/policies/development.toml` (new)

```toml
# Development profile — broad access for local development.
# NOT suitable for production or shared systems.

[[rules]]
id = "dev-allow-all-packages"
domains = ["env"]
actions = ["install"]
resource_pattern = "*"
effect = "allow"
priority = 10

[[rules]]
id = "dev-allow-all-builds"
domains = ["build"]
actions = ["*"]
resource_pattern = "*"
effect = "allow"
priority = 10

[[rules]]
id = "dev-allow-local-network"
domains = ["net"]
actions = ["http", "connect"]
resource_pattern = "localhost:*"
effect = "allow"
priority = 10

[[rules]]
id = "dev-deny-sensitive-paths"
domains = ["storage"]
actions = ["zone.create"]
resource_pattern = "/etc/**"
effect = "deny"
priority = 100  # High priority = checked first
```

**File:** `config/policies/production.toml` (new)

```toml
# Production profile — curated allowlists, escalation for unknowns.

[[rules]]
id = "prod-deny-system-packages"
domains = ["env"]
actions = ["install"]
resource_pattern = "*"
effect = "escalate"
priority = 1  # Default: everything requires approval

[[rules]]
id = "prod-allow-curated-python"
domains = ["env"]
actions = ["install"]
resource_pattern = "python:flask|django|fastapi|requests|numpy|pandas|pytest"
effect = "allow"
priority = 10

# ... similar for other ecosystems
```

**File:** `config/policies/restricted.toml` (new)

```toml
# Restricted profile — minimal access, everything escalated or denied.

[[rules]]
id = "restricted-deny-all-packages"
domains = ["env"]
actions = ["install"]
resource_pattern = "*"
effect = "escalate"
priority = 100

[[rules]]
id = "restricted-deny-all-processes"
domains = ["proc"]
actions = ["spawn"]
resource_pattern = "*"
effect = "escalate"
priority = 100

[[rules]]
id = "restricted-deny-all-network"
domains = ["net"]
actions = ["*"]
resource_pattern = "*"
effect = "deny"
priority = 100
```

### 3. CLI commands

**File:** `crates/agentos-cli/src/commands/capability.rs` (new)

```
agentos capability list              # List all active ephemeral grants
agentos capability grants <agent-id> # List grants for a specific agent
agentos capability revoke <grant-id> # Revoke a specific grant
agentos capability revoke-all <agent-id>  # Revoke all grants for agent
agentos capability policy list       # List all active policy rules
agentos capability policy test       # Dry-run: test a hypothetical request
    --domain env --action install --resource flask
agentos capability policy profile    # Switch policy profile
    --profile production
agentos capability stats             # Summary: grants active, denied, escalated
```

**File:** `crates/agentos-bus/src/message.rs`

Add `KernelCommand` variants:
```rust
ListCapabilityGrants { agent_id: Option<AgentID> },
RevokeCapabilityGrant { grant_id: String },
RevokeAllCapabilityGrants { agent_id: AgentID },
ListPolicyRules,
TestPolicyRule { domain: String, action: String, resource: String },
SetPolicyProfile { profile: String },
CapabilityStats,
```

### 4. Policy reload via ConfigWatcher

When `ConfigWatcher` detects config file change:
1. Reload policy rules from active profile
2. Log `KernelConfigChanged` audit event with details
3. New requests use updated rules (existing grants unaffected)

### 5. Kernel config section

**File:** `config/default.toml` (add section)

```toml
[capabilities.policy]
# Active policy profile: "development", "production", "restricted", or "custom"
active_profile = "development"

# Custom policy file path (used when active_profile = "custom")
custom_policy_file = ""

# Default TTL for ephemeral grants (seconds)
default_grant_ttl_secs = 3600  # 1 hour

# Maximum grants per agent
max_grants_per_agent = 50

# Default effect when no policy rule matches
default_effect = "escalate"  # fail-safe: require human approval
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/policy_engine.rs` | NEW — `PolicyEngine`, `PolicyRule`, evaluation logic |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod policy_engine;` |
| `crates/agentos-kernel/src/kernel.rs` | Initialize policy engine, wire to ConfigWatcher |
| `crates/agentos-kernel/src/capability_broker.rs` | Use `PolicyEngine` for evaluation instead of inline checks |
| `crates/agentos-cli/src/commands/capability.rs` | NEW — CLI commands |
| `crates/agentos-cli/src/main.rs` | Register capability subcommand |
| `crates/agentos-bus/src/message.rs` | Add KernelCommand variants |
| `crates/agentos-kernel/src/commands/capability.rs` | NEW — command handlers |
| `crates/agentos-kernel/src/run_loop.rs` | Add dispatch arms for capability commands |
| `config/default.toml` | Add `[capabilities.policy]` section |
| `config/policies/development.toml` | NEW — dev profile |
| `config/policies/production.toml` | NEW — prod profile |
| `config/policies/restricted.toml` | NEW — restricted profile |

---

## Dependencies

- **Requires:** Phase 1 (trait), Phase 7 (broker)
- **Blocks:** Nothing (final phase)

---

## Test Plan

- [ ] Policy engine evaluates rules in priority order (highest first)
- [ ] Deny rules override Allow rules at same priority
- [ ] Default effect is Escalate when no rule matches
- [ ] `dry_run()` returns correct effect without side effects
- [ ] CLI `capability list` shows active grants
- [ ] CLI `capability revoke` removes a grant
- [ ] CLI `capability policy test` returns evaluation result
- [ ] CLI `capability policy profile --profile production` switches profile
- [ ] Config reload updates policy rules without restart
- [ ] Existing grants survive policy reload (only new requests affected)
- [ ] Development profile allows broad access
- [ ] Production profile requires approval for uncurated packages
- [ ] Restricted profile denies all network access
- [ ] Policy file validation: malformed TOML returns clear error
- [ ] `max_grants_per_agent` enforced
- [ ] `default_grant_ttl_secs` applied to all auto-grants

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- policy_engine
cargo test -p agentos-cli -- capability
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# Integration test: switch profiles
agentos capability policy profile --profile development
agentos capability policy test --domain env --action install --resource flask
# Should return: Allow

agentos capability policy profile --profile restricted
agentos capability policy test --domain net --action http --resource api.github.com
# Should return: Deny
```

---

## Related

- [[01-capability-provider-trait]] — prerequisite
- [[07-dynamic-capability-negotiation]] — broker that this engine powers
- [[Kernel Mediated Capabilities Plan]]
