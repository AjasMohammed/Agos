---
title: "Phase 9: Config & Manifests Review"
tags:
  - review
  - config
  - phase-9
date: 2026-03-13
status: complete
effort: 30m
priority: medium
---

# Phase 9: Config & Manifests Review

> Review the default configuration and all tool manifest files for secure defaults.

---

## Why This Phase

Insecure defaults are a common source of vulnerabilities. If the default config is too permissive (high timeouts, disabled checks, open permissions), deployments will inherit those weaknesses. Tool manifests define the trust level and permission requirements for every built-in tool.

---

## Step 9.1 — Config & Tool Manifests (~300 lines)

**Files to read:**

| File | What It Contains |
|------|-----------------|
| `config/default.toml` | Kernel, LLM, vault, audit, tools config |
| `tools/core/shell-exec.toml` | Shell execution tool manifest |
| `tools/core/file-reader.toml` | File read tool manifest |
| `tools/core/file-writer.toml` | File write tool manifest |
| `tools/core/http-client.toml` | HTTP client tool manifest |
| `tools/core/data-parser.toml` | Data parser tool manifest |
| `tools/core/memory-search.toml` | Memory search tool manifest |
| `tools/core/memory-write.toml` | Memory write tool manifest |

**Checklist:**
- [ ] Default timeouts are reasonable (not too long)
- [ ] Default task/agent limits prevent resource exhaustion
- [ ] LLM config does not include default API keys
- [ ] Vault config uses strong key derivation defaults
- [ ] Audit retention is set appropriately
- [ ] All tool manifests have `trust_tier = "core"`
- [ ] Tool permission sets are minimal (least privilege)
- [ ] No secrets or credentials in config files
- [ ] Sandbox policy defaults are restrictive

---

## Findings

| File | Line(s) | Severity | Category | Description | Fix Applied |
|------|---------|----------|----------|-------------|-------------|
| `config/default.toml` | 13 | WARNING | Security default | `vault_path = "/tmp/agentos/vault/secrets.db"` — `/tmp` is world-listable on multi-user systems; any local process can discover the vault file path | Yes — added security warning comment + kernel creates parent dir with `0o700` permissions |
| `config/default.toml` | 17 | INFO | Config documentation | `log_path = "/tmp/agentos/data/audit.db"` — same concern as vault, audit DB also in `/tmp` | Documented in config comment |

## Remaining Issues

| Issue | Severity | Notes |
|-------|----------|-------|
| Default `/tmp` paths | INFO | Development-safe; production config (`config/production.toml`) should use `~/.agentos/` paths. Kernel now enforces `0o700` on vault dir. |

## Files Changed

| File | Change |
|------|--------|
| `config/default.toml` | Security warning comments on `/tmp` vault and audit paths |
| `crates/agentos-kernel/src/kernel.rs` | Vault parent dir created with `0o700` at boot |

## Dependencies

None — can run independently or after Phase 1.

## Verification

```bash
# Confirm config parses correctly
cargo test -p agentos-kernel -- config
```

---

## Related

- [[Full Codebase Review Plan]]
- [[10-synthesis-and-report]]
