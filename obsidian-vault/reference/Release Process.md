---
title: Release Process
tags:
  - release
  - versioning
  - reference
date: 2026-03-16
status: complete
---

# Release Process

> Defines the AgentOS release lifecycle — from commit to tagged artifact — including cut criteria, versioning rules, and the sign-off gate.

---

## Overview

AgentOS follows **semantic versioning** (SemVer 2.0): `vMAJOR.MINOR.PATCH`.

| Version Component | When to bump |
|---|---|
| `MAJOR` | Breaking API or protocol change (bus wire format, config schema, CLI flag removal) |
| `MINOR` | Backward-compatible new feature (new CLI command, new tool capability) |
| `PATCH` | Bug fix or internal improvement with no observable API change |

Pre-v1 releases use `v0.x.y`. The first stable release is `v0.1.0`.

---

## Cut Criteria — Required Before Any Tag

A release candidate **must not be tagged** until all of the following are true:

### Quality Gate

```bash
cargo fmt --all -- --check     # must exit 0
cargo clippy --workspace -- -D warnings  # must exit 0 with zero warnings
cargo test --workspace         # all tests must pass
cargo build --release --workspace  # release build must succeed
```

### Security Gate

All 7 security acceptance scenarios must pass:

```bash
cargo test -p agentos-kernel --test security_acceptance_test
```

Refer to `docs/guide/06-security.md` — "Deployment Security Acceptance" for the scenario table.

### Container Gate

```bash
docker build -t agentos:candidate .
docker run --rm agentos:candidate --help
# health endpoint responds with 200
curl -sf http://localhost:9091/healthz
```

### Evidence Checklist

Before creating the tag, the releaser must verify and record:

- [ ] `cargo test --workspace` — all N tests passed (include count)
- [ ] `cargo clippy` — zero warnings
- [ ] `cargo build --release` — build succeeded, binary size noted
- [ ] Security acceptance test — 7/7 scenarios passed
- [ ] Docker build + healthcheck — passed
- [ ] `config/production.toml` reviewed — no `/tmp` paths
- [ ] `CHANGELOG.md` entry added (if present)
- [ ] `Cargo.toml` workspace version updated
- [ ] PR merged to `main` with all CI checks green

---

## Versioning Workflow

### Step 1 — Bump the Version

Update the version in the workspace `Cargo.toml`:

```toml
[workspace.package]
version = "0.1.0"
```

All crates that inherit `workspace.package.version` will pick this up automatically. Crates with pinned versions must be updated manually.

### Step 2 — Commit the Bump

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to v0.1.0"
```

### Step 3 — Tag the Release

```bash
git tag -a v0.1.0 -m "Release v0.1.0 — first stable deployment"
git push origin v0.1.0
```

Tags must:
- Use annotated tags (`-a`), not lightweight tags
- Include a meaningful message describing the release milestone
- Be created on `main` only after the PR has been merged and CI is green
- Never be force-pushed or deleted post-publication

### Step 4 — Create GitHub Release

Create a GitHub Release from the tag with:
- **Title:** `AgentOS v0.1.0`
- **Body:** Summary of changes, known issues, upgrade notes
- **Assets:** pre-built binary (optional), Docker image tag

---

## Rollback Procedure

If a deployed release causes a critical regression:

1. **Identify the last known-good tag** — `git tag --sort=-version:refname | head -5`
2. **Redeploy from the previous tag:**
   ```bash
   git checkout v0.0.9  # last known-good tag
   cargo build --release --workspace
   # or: docker pull agentos:v0.0.9
   ```
3. **Check data compatibility:**
   - Vault schema: check `crates/agentos-vault/src/vault.rs` migration steps
   - Audit log schema: check `crates/agentos-audit/src/log.rs` migration steps
   - If a downgrade crosses a schema migration, a manual SQLite rollback may be required
4. **Trigger conditions for rollback:**
   - Kernel panic on startup
   - Vault decryption failure (existing data unreadable)
   - Security acceptance test regression (any of the 7 scenarios fail)
   - > 10% increase in `cargo test` failure rate vs. prior release
5. **Sign-off for rollback:** Rollback decisions must be documented — who triggered it, which tag, and why. Use the sign-off template in [[First Deployment Runbook]].

---

## Sign-Off Template

Copy into the PR or release notes before tagging:

```markdown
## Release Sign-Off — vX.Y.Z

**Date:** YYYY-MM-DD
**Releaser:** @<github-handle>
**Commit:** <sha>

### Evidence

| Gate | Result | Notes |
|---|---|---|
| cargo test | ✅ PASS / ❌ FAIL | N tests, M skipped |
| cargo clippy | ✅ PASS / ❌ FAIL | — |
| cargo build --release | ✅ PASS / ❌ FAIL | binary: X MB |
| security_acceptance_test | ✅ 7/7 / ❌ N/7 | — |
| Docker build + healthcheck | ✅ PASS / ❌ FAIL | — |

### Known Issues
<!-- List any deferred issues with tracking links -->

### Approved
- [ ] Releaser
- [ ] (Optional) Second reviewer
```

---

## CI / Automation

The `.github/workflows/ci.yml` and `.github/workflows/release-gate.yml` pipelines run on every push and PR. A release tag **must not be created** while any CI job is red.

The `release-gate.yml` workflow runs the full quality gate (fmt + clippy + test + release build) on every push to `main` and PRs. When creating a release tag, trigger this workflow manually or verify all steps pass on the tagged commit before announcing the release. A GitHub Release can then be created manually from the tag.

---

## Related

- [[First Deployment Runbook]] — First-boot procedure and smoke checklist
- `docs/guide/06-security.md` — Security acceptance scenarios
- `docs/guide/07-configuration.md` — Production configuration reference
- `agentic-os-deployment.md` — Deployment architecture and upgrade strategy
