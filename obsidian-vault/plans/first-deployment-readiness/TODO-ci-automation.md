---
title: "TODO: Create CI Automation Workflow"
tags:
  - ci
  - deployment
  - next-steps
date: 2026-03-17
status: planned
effort: 2h
priority: high
---

# Create CI Automation Workflow

> Create a GitHub Actions CI workflow that runs `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace` on every push and pull request so quality gates cannot regress.

## Why This Phase

The First Deployment Readiness Plan (Phase 01) specifies that CI enforcement is mandatory: "quality gates must be automated in a workflow file to prevent regression." The plan references `.github/workflows/release-gate.yml` and a CI workflow, but no workflow file has been confirmed to exist. Without CI automation, quality gates (fmt, clippy, tests) can silently break between manual runs.

The TODO-release-cutover.md also references this file as required before cutting v0.1.0.

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| CI workflow | None confirmed | `.github/workflows/ci.yml` runs on push and PR |
| fmt enforcement | Manual | Automated — fails CI on formatting violations |
| clippy enforcement | Manual | Automated — `-D warnings` flag, fails on any warning |
| test enforcement | Manual | Automated — full workspace test suite |
| Release gate | None | `.github/workflows/release-gate.yml` for tag pushes |

## Detailed Subtasks

1. Check if `.github/workflows/` directory exists:
   ```bash
   ls -la .github/workflows/ 2>/dev/null || echo "Does not exist"
   ```

2. If the directory does not exist, create it:
   ```bash
   mkdir -p .github/workflows
   ```

3. Create `.github/workflows/ci.yml` with the following content:

   ```yaml
   name: CI

   on:
     push:
       branches: [ main ]
     pull_request:
       branches: [ main ]

   env:
     CARGO_TERM_COLOR: always
     RUST_BACKTRACE: 1

   jobs:
     check:
       name: Check (fmt + clippy + test)
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4

         - name: Install Rust toolchain
           uses: dtolnay/rust-toolchain@stable
           with:
             components: rustfmt, clippy

         - name: Cache cargo registry
           uses: actions/cache@v4
           with:
             path: |
               ~/.cargo/registry
               ~/.cargo/git
               target
             key: ${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}
             restore-keys: |
               ${{ runner.os }}-cargo-

         - name: Check formatting
           run: cargo fmt --all -- --check

         - name: Run clippy
           run: cargo clippy --workspace -- -D warnings

         - name: Run tests
           run: cargo test --workspace
   ```

4. Optionally create `.github/workflows/release-gate.yml` for tag-based releases (see `05-release-process-and-cutover.md`).

5. Commit and push to verify the workflow triggers correctly.

## Files Changed

| File | Change |
|------|--------|
| `.github/workflows/ci.yml` | Create — CI workflow for push and PR |

## Dependencies

- Quality gates must pass locally before pushing (fmt, clippy, tests). Run:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace -- -D warnings
  cargo test --workspace
  ```
- If any gate fails, fix it before creating the CI workflow (otherwise CI will immediately fail).

## Test Plan

1. After creating the workflow file, push to the repository.
2. Open the repository on GitHub and check the Actions tab.
3. Verify the CI job runs and passes.
4. Introduce a deliberate clippy warning (e.g., `let _x = vec![1, 2, 3];`) in a test file, push — CI should fail.
5. Revert and confirm CI passes again.

## Verification

```bash
# Verify file exists
ls -la .github/workflows/ci.yml

# Verify local gates pass before pushing
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

## Related

- [[First Deployment Readiness Plan]] — master plan (Phase 01)
- [[01-quality-gates-stabilization]] — quality gates phase
- [[TODO-release-cutover]] — release process that depends on CI
- [[audit_report]] — identified this as a high-priority gap
