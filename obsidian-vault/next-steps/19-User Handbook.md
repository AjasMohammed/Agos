---
title: User Handbook
tags:
  - docs
  - v3
  - next-steps
date: 2026-03-13
status: planned
effort: 5d
priority: high
---

# User Handbook

> Create a comprehensive 19-chapter user handbook covering every aspect of AgentOS from installation to troubleshooting, written as Obsidian markdown in `obsidian-vault/reference/handbook/`.

---

## Current State
Existing documentation is scattered across `docs/guide/` (7 files, V1/V2 era) and `obsidian-vault/reference/` (12 internal reference files). No unified user-facing handbook exists. Many V3 features (event system, cost tracking, escalation, identity, snapshots, resource arbitration) have zero user documentation.

## Goal / Target State
A self-contained handbook of 19 chapters plus an index, located at `obsidian-vault/reference/handbook/`. Each chapter is readable standalone and covers one major area of AgentOS with commands, examples, configuration, and conceptual explanations. The CLI reference chapter documents all 18 command groups with every subcommand and flag.

## Phases

All phase detail files are in `obsidian-vault/plans/user-handbook/`.

| # | Phase | Output Files | Status |
|---|-------|-------------|--------|
| 01 | [[01-foundation-chapters]] | `01-Introduction and Philosophy.md`, `02-Installation and First Run.md`, `03-Architecture Overview.md` | planned |
| 02 | [[02-cli-reference]] | `04-CLI Reference Complete.md` | planned |
| 03 | [[03-agent-and-task-system]] | `05-Agent Management.md`, `06-Task System.md` | planned |
| 04 | [[04-tool-system]] | `07-Tool System.md`, `17-WASM Tools Development.md` | planned |
| 05 | [[05-security-and-vault]] | `08-Security Model.md`, `09-Secrets and Vault.md` | planned |
| 06 | [[06-memory-system]] | `10-Memory System.md` | planned |
| 07 | [[07-pipeline-event-cost]] | `11-Pipeline and Workflows.md`, `12-Event System.md`, `13-Cost Tracking.md` | planned |
| 08 | [[08-audit-config-advanced]] | `14-Audit Log.md`, `15-LLM Configuration.md`, `16-Configuration Reference.md`, `18-Advanced Operations.md` | planned |
| 09 | [[09-troubleshooting-and-index]] | `19-Troubleshooting and FAQ.md`, `AgentOS Handbook Index.md` | planned |

## Verification
```bash
# All handbook files exist
ls obsidian-vault/reference/handbook/*.md | wc -l
# Should output: 20 (19 chapters + 1 index)

# No broken wikilinks within the handbook
grep -r '\[\[' obsidian-vault/reference/handbook/ | grep -v '\.md\]' || true
```

## Related
- [[User Handbook Plan]]
- [[CLI Reference]]
- [[Security Model]]
- [[Memory System]]
