---
title: Phase 4 — Agent Manual Integration
tags:
  - kernel
  - tools
  - agent-manual
  - phase-4
date: 2026-04-14
status: planned
effort: 0.5d
priority: medium
---

# Phase 4 — Agent Manual Integration

> Add `ManualSection::Scripts` to the agent manual so agents understand what Script Tools are, how to call them, and when to prefer them over built-in tools.

---

## Files to Modify

| File | Change |
|---|---|
| `crates/agentos-tools/src/agent_manual.rs` | Add `Scripts` variant, `from_str`, `all_names`, `fn section_scripts`, dispatch arm |

---

## Changes to `agent_manual.rs`

See [[04-agent-manual-integration]] for the exact diff — this phase is implemented in this same conversation.

### Summary of changes:
1. `ManualSection::Scripts` variant added to enum
2. `"scripts"` arm in `from_str()`
3. `"scripts"` in `all_names()` slice
4. `ManualSection::Scripts => self.section_scripts()` in the dispatch match
5. `fn section_scripts()` returning the full JSON documentation

---

## Content of `section_scripts()`

The section must tell agents:
- What script tools are and how they differ from built-in tools
- The I/O contract: `AGENTOS_INPUT` env var, stdout JSON
- How to call them (same interface as any tool — just use the tool name)
- What languages are supported
- How to ask a human to add one (`ask-user` tool)
- What happens if the script fails

This is detailed in the implementation below (see agent_manual.rs changes in this PR).

---

## Verification

```bash
cargo test -p agentos-tools agent_manual
cargo build -p agentos-tools
```

Manual verification:
```bash
agentos manual scripts
# Should output JSON section with title "Script Tools"
```
