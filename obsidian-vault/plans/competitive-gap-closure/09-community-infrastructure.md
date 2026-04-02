---
title: "Phase 3.3: Community Infrastructure"
tags:
  - docs
  - community
  - v3
  - plan
  - phase-3
date: 2026-03-30
status: complete
effort: 2d
priority: medium
---

# Phase 3.3: Community Infrastructure

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create documentation site, CONTRIBUTING.md, CI/CD pipeline, issue templates, and release workflow — the non-code infrastructure needed for an open-source community.

**Architecture:** mdBook for docs site (Rust-native, no Node). GitHub Actions for CI (build → test → clippy → fmt → bench). Conventional commits with git-cliff for CHANGELOG. Issue and PR templates.

**Tech Stack:** mdBook, GitHub Actions, git-cliff

---

## Why This Phase

No one contributes to a project they can't understand. AgentOS has 17 crates with no contributor guide. OpenClaw has 1,075 contributors and weekly open-source meetings. Community infrastructure is what turns a solo project into an ecosystem.

## Current → Target State

**Current:** No docs site, no CONTRIBUTING.md, no CI, no issue templates, no release workflow.

**Target:** Full contributor pipeline: docs, templates, CI, release automation.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `docs/book/` | Create | mdBook documentation source |
| `docs/book/src/SUMMARY.md` | Create | mdBook table of contents |
| `docs/book/src/quickstart.md` | Create | 30-second install to first task |
| `docs/book/src/architecture.md` | Create | Architecture overview |
| `docs/book/src/api-reference.md` | Create | REST API reference |
| `docs/book/src/skills-guide.md` | Create | How to write a skill |
| `docs/book/src/tools-guide.md` | Create | How to write a tool |
| `docs/book/src/channels-guide.md` | Create | How to write a channel adapter |
| `docs/book/src/providers-guide.md` | Create | How to add an LLM provider |
| `docs/book/book.toml` | Create | mdBook config |
| `CONTRIBUTING.md` | Create | Contributor guide |
| `SECURITY.md` | Create | Security policy |
| `.github/workflows/ci.yml` | Create | CI pipeline |
| `.github/ISSUE_TEMPLATE/bug.yml` | Create | Bug report template |
| `.github/ISSUE_TEMPLATE/feature.yml` | Create | Feature request template |
| `.github/PULL_REQUEST_TEMPLATE.md` | Create | PR template |
| `cliff.toml` | Create | git-cliff config for CHANGELOG |

## Dependencies

- **Requires:** Phase 3.1 (Single binary — install instructions reference it)
- **Blocks:** Nothing

---

## Detailed Tasks

### Task 1: mdBook Documentation Site

- [ ] Initialize mdBook:
```bash
cargo install mdbook
mkdir -p docs/book/src
```

- [ ] Write `docs/book/book.toml`:
```toml
[book]
title = "AgentOS Documentation"
authors = ["AgentOS Contributors"]
language = "en"
multilingual = false
src = "src"

[build]
build-dir = "../../target/book"
```

- [ ] Write `docs/book/src/SUMMARY.md`:
```markdown
# Summary

- [Quick Start](quickstart.md)
- [Architecture](architecture.md)
- [API Reference](api-reference.md)
- [Guides](guides/README.md)
  - [Writing Skills](guides/skills.md)
  - [Writing Tools](guides/tools.md)
  - [Channel Adapters](guides/channels.md)
  - [LLM Providers](guides/providers.md)
- [Security Model](security.md)
- [Benchmarks](benchmarks.md)
- [Deployment](deployment.md)
```

- [ ] Write quick start guide (install → `agentos start` → connect agent → run task)
- [ ] Write architecture overview (17 crates, kernel flow, security model)
- [ ] Write extension guides (skill, tool, channel, provider — each with minimal working example)
- [ ] Commit

### Task 2: CONTRIBUTING.md

- [ ] Write CONTRIBUTING.md covering:
  - Architecture map (which crate does what — 1 paragraph each)
  - Development setup (`cargo build --workspace`, `cargo test --workspace`)
  - PR process: fork → branch → test → clippy → fmt → PR
  - "Good first issues" label guide
  - How to add: a tool (1 file), a skill (2 files), a channel adapter (1 file), a provider (1 file or 1 TOML entry)
- [ ] Commit

### Task 3: CI Pipeline

- [ ] Write `.github/workflows/ci.yml`:
```yaml
name: CI
on: [push, pull_request]
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build
        run: cargo build --workspace
      - name: Test
        run: cargo test --workspace
      - name: Clippy
        run: cargo clippy --workspace -- -D warnings
      - name: Format
        run: cargo fmt --all -- --check
```
- [ ] Commit

### Task 4: Issue and PR Templates

- [ ] Write `.github/ISSUE_TEMPLATE/bug.yml` (structured: description, steps, expected, actual, version)
- [ ] Write `.github/ISSUE_TEMPLATE/feature.yml` (structured: problem, proposal, alternatives)
- [ ] Write `.github/PULL_REQUEST_TEMPLATE.md` (checklist: tests pass, clippy clean, docs updated)
- [ ] Commit

### Task 5: SECURITY.md and Release Automation

- [ ] Write `SECURITY.md` with responsible disclosure process
- [ ] Write `cliff.toml` for conventional commit changelog generation
- [ ] Add `CHANGELOG.md` generation step to release workflow
- [ ] Commit

## Verification

```bash
# Build docs
cd docs/book && mdbook build && mdbook serve
# Open http://localhost:3000

# Verify CI config
act -j check  # If act is installed, or push to GitHub
```
