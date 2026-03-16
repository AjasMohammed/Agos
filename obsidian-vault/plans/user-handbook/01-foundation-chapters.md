---
title: Handbook Foundation Chapters
tags:
  - docs
  - v3
  - plan
date: 2026-03-13
status: planned
effort: 4h
priority: high
---

# Handbook Foundation Chapters

> Write the first three chapters of the AgentOS User Handbook: Introduction and Philosophy, Installation and First Run, and Architecture Overview.

---

## Why This Subtask
These three chapters form the foundation that all other handbook chapters build on. A user must understand what AgentOS is, how to install it, and how the architecture fits together before diving into any specific subsystem. These chapters must be completed first because all subsequent chapters assume the reader has this context.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Introduction | `docs/guide/01-introduction.md` exists but mentions only V1/V2 status | New `01-Introduction and Philosophy.md` covering full V3 vision with Linux analogy table, core principles, and crate overview |
| Getting Started | `docs/guide/02-getting-started.md` exists but lacks production setup | New `02-Installation and First Run.md` with prerequisites, build, dev config, production config, Docker basics, first agent connection, first task |
| Architecture | `docs/guide/03-architecture.md` exists but missing V3 subsystems | New `03-Architecture Overview.md` with full crate dependency graph (17 crates), kernel boot sequence, intent flow, memory architecture, event bus, cost tracker, escalation system |

---

## What to Do

### 1. Create the handbook directory

```bash
mkdir -p obsidian-vault/reference/handbook
```

### 2. Write `01-Introduction and Philosophy.md`

Read these source files for ground truth:
- `docs/guide/01-introduction.md` -- existing intro content to incorporate
- `CLAUDE.md` -- project overview section

The chapter must include:
- **What is AgentOS** -- one-paragraph definition
- **Core Principles** table -- 6 principles (security, minimal, LLM-native, multi-LLM, social agents, community extensible)
- **Linux <-> AgentOS Analogy** -- full mapping table (Kernel -> Inference Kernel, Process -> Agent Task, Syscall -> Intent, etc.)
- **How AgentOS differs from traditional AI frameworks** -- comparison with LangChain, CrewAI, etc.
- **Crate overview** -- table of all 17 crates with one-line descriptions
- **Current status** -- V3 feature completion summary
- **How to read this handbook** -- reading order guidance

Use this frontmatter:
```yaml
---
title: Introduction and Philosophy
tags:
  - docs
  - handbook
date: 2026-03-13
status: planned
---
```

### 3. Write `02-Installation and First Run.md`

Read these source files for ground truth:
- `docs/guide/02-getting-started.md` -- existing getting started content
- `config/default.toml` -- all dev configuration values
- `config/production.toml` -- all production configuration values
- `crates/agentos-cli/src/main.rs` -- `cmd_start()` function for boot sequence, CLI entry point arguments
- `Cargo.toml` (workspace root) -- Rust edition, workspace members

The chapter must include:
- **Prerequisites** -- Rust 1.75+, Linux, optional Ollama/API keys
- **Building from source** -- `cargo build --workspace`, `cargo test --workspace`
- **Development configuration** -- walkthrough of `config/default.toml` with every key explained
- **Production configuration** -- walkthrough of `config/production.toml` with every key explained
- **Starting the kernel** -- `agentctl start`, vault passphrase prompt, what happens during boot
- **Connecting your first agent** -- Ollama, OpenAI, Anthropic, Gemini examples
- **Running your first task** -- `agentctl task run` example
- **Quick example session** -- complete end-to-end session transcript
- **Migration from dev to production** -- checklist with directory creation and config swap

### 4. Write `03-Architecture Overview.md`

Read these source files for ground truth:
- `docs/guide/03-architecture.md` -- existing architecture content
- `crates/agentos-kernel/src/kernel.rs` -- `Kernel::boot()` for boot sequence
- `crates/agentos-kernel/src/run_loop.rs` -- main event loop
- `crates/agentos-kernel/src/router.rs` -- routing strategies
- `crates/agentos-kernel/src/context.rs` -- context window management
- `crates/agentos-kernel/src/cost_tracker.rs` -- cost tracking overview
- `crates/agentos-kernel/src/escalation.rs` -- escalation system
- `crates/agentos-kernel/src/event_bus.rs` -- event bus
- `crates/agentos-memory/src/lib.rs` -- memory tier exports

The chapter must include:
- **System architecture diagram** -- ASCII art showing all major components (kernel, CLI, bus, LLM adapters, tools, security, memory)
- **Crate dependency graph** -- tree showing all 17 crates and their dependencies
- **Kernel boot sequence** -- numbered list of every subsystem initialization step
- **Intent flow** -- step-by-step walkthrough from CLI command to LLM response (12 steps)
- **Task routing engine** -- 4 routing strategies with description table
- **Memory architecture** -- 3 tiers (working, episodic, semantic) with description
- **Agent Message Bus** -- direct, delegation, broadcast modes
- **Event system architecture** -- event emission, subscription, filtering, throttling, triggered tasks
- **Security layers** -- 7-layer defense-in-depth table
- **Cost tracking architecture** -- per-agent budgets, model downgrade, budget enforcement

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/01-Introduction and Philosophy.md` | Create new |
| `obsidian-vault/reference/handbook/02-Installation and First Run.md` | Create new |
| `obsidian-vault/reference/handbook/03-Architecture Overview.md` | Create new |

---

## Prerequisites
None -- this is the first subtask.

---

## Test Plan
- All three files exist in `obsidian-vault/reference/handbook/`
- Each file has valid YAML frontmatter
- Each file has all required sections listed above
- All configuration keys from `config/default.toml` and `config/production.toml` are documented in the Installation chapter
- The Linux <-> AgentOS analogy table is present in Introduction
- The crate dependency graph covers all 17 crates

---

## Verification
```bash
# Files exist
test -f obsidian-vault/reference/handbook/01-Introduction\ and\ Philosophy.md
test -f obsidian-vault/reference/handbook/02-Installation\ and\ First\ Run.md
test -f obsidian-vault/reference/handbook/03-Architecture\ Overview.md

# Each has frontmatter
head -1 obsidian-vault/reference/handbook/01-*.md | grep "^---"
head -1 obsidian-vault/reference/handbook/02-*.md | grep "^---"
head -1 obsidian-vault/reference/handbook/03-*.md | grep "^---"

# Architecture mentions all 17 crates
grep -c "agentos-" obsidian-vault/reference/handbook/03-Architecture\ Overview.md
# Should be >= 17
```
