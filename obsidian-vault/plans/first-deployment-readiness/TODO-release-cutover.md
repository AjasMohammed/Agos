---
title: "TODO: Execute Release Process and Cut v0.1.0 Tag"
tags:
  - deployment
  - release
  - next-steps
date: 2026-03-17
status: planned
effort: 4h
priority: high
---

# Execute Release Process and Cut v0.1.0 Tag

> Complete Phase 05 of First Deployment Readiness: verify the release gate workflow passes, create the v0.1.0 git tag, and generate the signed-off release checklist.

## Why This Phase

Phases 00-04 of the First Deployment Readiness plan are complete: code safety hardening (no panics/lock-poisoning), quality gates (fmt, clippy, tests all pass), production config profile, containerization artifacts (Dockerfile, docker-compose.yml), and security gate closure. Phase 05 is the final step: cutting the first immutable release tag from a fully validated commit.

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Release tag | No tags | `v0.1.0` tag on the validated commit |
| Release gate CI | Exists (`.github/workflows/release-gate.yml`) | Passes clean end-to-end |
| Launch checklist | No signed-off checklist | `obsidian-vault/plans/first-deployment-readiness/LAUNCH-CHECKLIST.md` with sign-offs |
| Docker image | Not published | Image buildable from `docker-compose.yml` |

## Detailed Subtasks

1. Run the full release gate locally to confirm all checks pass:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace -- -D warnings
   cargo test --workspace
   cargo build --workspace --release
   ```

2. Read `obsidian-vault/plans/first-deployment-readiness/05-release-process-and-cutover.md` for the full checklist

3. Create `obsidian-vault/plans/first-deployment-readiness/LAUNCH-CHECKLIST.md` documenting each gate with a sign-off:
   - `cargo fmt` clean: ✅
   - `cargo clippy` clean: ✅
   - `cargo test` all pass: ✅
   - `cargo build --release` succeeds: ✅
   - Docker build succeeds: (verify with `docker build -t agentos:test .`)
   - Production config validated (no `/tmp` paths in `config/production.toml`): ✅
   - Security smoke tests pass: (run from `04-security-gate-closure.md`)
   - No hardcoded secrets in source: (verify with `grep -r "password\|secret\|token" config/ --include="*.toml"`)

4. Tag the release:
   ```bash
   git tag -a v0.1.0 -m "First deployment baseline — all quality gates pass"
   ```

5. Update `obsidian-vault/plans/first-deployment-readiness/First Deployment Readiness Plan.md` to `status: complete`

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/plans/first-deployment-readiness/LAUNCH-CHECKLIST.md` | Create with sign-offs for all release gates |
| `obsidian-vault/plans/first-deployment-readiness/First Deployment Readiness Plan.md` | Update `status: planned` → `status: complete` |
| `obsidian-vault/plans/first-deployment-readiness/05-release-process-and-cutover.md` | Update `status: planned` → `status: complete` |

## Dependencies

Phases 00-04 must be complete (they are — confirmed by plan docs and code inspection).

## Test Plan

- All 4 quality gates pass (fmt, clippy, test, release build)
- Docker build succeeds locally
- `git tag` creates the tag at HEAD

## Verification

```bash
# Quality gates
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --workspace --release

# Docker
docker build -t agentos:v0.1.0 .

# Tag exists
git tag | grep v0.1.0
```

## Related

- [[First Deployment Readiness Plan]] — master plan
- [[05-release-process-and-cutover]] — detailed phase spec
- [[audit_report]] — GAP-H04
