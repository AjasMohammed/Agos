---
title: "Phase 2.2: Pre-built Agents"
tags:
  - skills
  - security
  - v3
  - plan
  - phase-2
date: 2026-03-30
status: planned
effort: 3d
priority: high
---

# Phase 2.2: Pre-built Agents

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship 7 pre-built skills in `skills/core/` — 5 security/ops agents that leverage AgentOS's unique subsystems, plus 2 general-purpose agents.

**Architecture:** Each agent is a skill directory with `SKILL.toml` + `prompt.md`. They use existing tools and subsystems (audit log, HAL, injection scanner, cost tracker). The `SkillRegistry` (Phase 2.1) loads them at boot and arms their triggers.

**Tech Stack:** SKILL.toml format (Phase 2.1), existing AgentOS tools

---

## Why This Phase

OpenFang ships 7 "Hands." OpenClaw has 100+ skills. AgentOS ships zero pre-built agents. The security/ops agents are the key differentiator — they leverage audit logs (83+ event types, Merkle verification), HAL (device discovery/gating), injection scanning, and cost tracking that no competitor has at this depth.

## Current → Target State

**Current:** No pre-built agents. Tools exist but require manual composition.

**Target:** 7 skills in `skills/core/` loaded at boot. 5 scheduled (run autonomously), 2 on-demand.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `skills/core/compliance-auditor/SKILL.toml` | Create | Manifest |
| `skills/core/compliance-auditor/prompt.md` | Create | System prompt |
| `skills/core/secops-monitor/SKILL.toml` | Create | Manifest |
| `skills/core/secops-monitor/prompt.md` | Create | System prompt |
| `skills/core/infra-watcher/SKILL.toml` | Create | Manifest |
| `skills/core/infra-watcher/prompt.md` | Create | System prompt |
| `skills/core/cost-optimizer/SKILL.toml` | Create | Manifest |
| `skills/core/cost-optimizer/prompt.md` | Create | System prompt |
| `skills/core/backup-guardian/SKILL.toml` | Create | Manifest |
| `skills/core/backup-guardian/prompt.md` | Create | System prompt |
| `skills/core/researcher/SKILL.toml` | Create | Manifest |
| `skills/core/researcher/prompt.md` | Create | System prompt |
| `skills/core/browser-automator/SKILL.toml` | Create | Manifest |
| `skills/core/browser-automator/prompt.md` | Create | System prompt |

## Dependencies

- **Requires:** Phase 2.1 (Skills Abstraction — SkillRegistry, SKILL.toml format)
- **Blocks:** Nothing

---

## Detailed Tasks

### Task 1: Compliance Auditor

- [ ] **Step 1: Create directory and SKILL.toml**

```bash
mkdir -p skills/core/compliance-auditor
```

```toml
[skill]
name = "compliance-auditor"
version = "0.1.0"
description = "Monitors audit trail for policy violations, verifies Merkle chain integrity, generates compliance reports"
author = "agentos-core"
trust_tier = "core"

[triggers]
schedule = "0 */6 * * *"
events = ["permission_changed", "secret_accessed"]

[agent]
system_prompt_file = "prompt.md"
roles = ["security-monitor"]
default_provider = "anthropic"
default_model = "claude-sonnet-4-6"

[tools]
required = ["audit-query", "audit-verify", "notify-user", "memory-write"]
optional = ["http-client"]

[permissions]
required = ["audit:read", "notification:write", "memory:write"]

[budget]
max_cost_per_run = 0.50
max_tokens_per_run = 50000
```

- [ ] **Step 2: Write system prompt**

`prompt.md`:
```markdown
You are the Compliance Auditor for this AgentOS instance. Your job is to monitor the audit trail for policy violations and ensure system integrity.

## Your Responsibilities

1. **Audit Trail Review**: Query recent audit entries since your last run. Look for:
   - Failed permission checks (unauthorized access attempts)
   - Secret access without proper authorization
   - Permission changes (escalations, revocations)
   - Unusual patterns (same agent failing multiple times, rapid permission grants)

2. **Merkle Chain Verification**: Run audit-verify to confirm the audit log has not been tampered with. If verification fails, this is CRITICAL — notify immediately.

3. **Compliance Summary**: Write a brief summary of findings to memory. Include:
   - Total events reviewed
   - Violations found (with event IDs)
   - Merkle chain status (intact/broken)
   - Recommendations

4. **Notification**: If any violations are found, send a notification with priority "high". If the Merkle chain is broken, use priority "critical".

## Tools Available
- `audit-query`: Query audit log entries by time range, event type, agent
- `audit-verify`: Verify Merkle chain integrity from a sequence number
- `notify-user`: Send notifications to the user
- `memory-write`: Store findings in episodic memory for trend analysis

## Behavior
- Be thorough but concise in reports
- Always verify the Merkle chain — this is non-negotiable
- Prioritize security events over informational events
- If you find nothing unusual, still write a clean summary to memory
```

- [ ] **Step 3: Commit**

```bash
git add skills/core/compliance-auditor/
git commit -m "feat(skills): add compliance-auditor pre-built agent"
```

### Task 2: SecOps Monitor

- [ ] Create `skills/core/secops-monitor/SKILL.toml` with triggers: `task_completed` + hourly cron
- [ ] Write `prompt.md` focused on: injection detection review, taint tracking analysis, suspicious permission escalation patterns, SSRF attempt detection
- [ ] Tools: memory-search, audit-query, notify-user, escalation-status
- [ ] Commit

### Task 3: Infrastructure Watcher

- [ ] Create `skills/core/infra-watcher/SKILL.toml` with triggers: every 15min + `device_mounted`, `device_quarantined`
- [ ] Write `prompt.md` focused on: CPU/memory/disk/thermal monitoring, baseline comparison, anomaly flagging (>90% CPU, disk >85%), new device detection
- [ ] Tools: hardware-info, network-monitor, process-manager, notify-user, memory-write
- [ ] Commit

### Task 4: Cost Optimizer

- [ ] Create `skills/core/cost-optimizer/SKILL.toml` with triggers: daily 8am + budget soft-limit
- [ ] Write `prompt.md` focused on: per-agent/task/model spend analysis, model downgrade recommendations, retry rate analysis, cost trend tracking
- [ ] Tools: kernel cost APIs, memory-write, notify-user
- [ ] Commit

### Task 5: Backup Guardian

- [ ] Create `skills/core/backup-guardian/SKILL.toml` with triggers: daily 2am
- [ ] Write `prompt.md` focused on: audit log file freshness, snapshot recency (warn if >24h), vault backup state, SQLite integrity_check on memory DBs
- [ ] Tools: file-reader, shell-exec, audit-query, notify-user
- [ ] Commit

### Task 6: Researcher (General-Purpose)

- [ ] Create `skills/core/researcher/SKILL.toml` — on-demand only (no schedule/events)
- [ ] Write `prompt.md` focused on: multi-step web research, source cross-referencing, citation tracking, writing summaries to scratchpad
- [ ] Tools: web-fetch, http-client, memory-write, scratch-write, data-parser
- [ ] Commit

### Task 7: Browser Automator (General-Purpose)

- [ ] Create `skills/core/browser-automator/SKILL.toml` — on-demand only
- [ ] Write `prompt.md` focused on: headless browser navigation, form filling, data extraction, screenshot capture
- [ ] Tools: shell-exec, file-writer, data-parser, scratch-write
- [ ] Commit

### Task 8: Verify All Skills Load

- [ ] **Step 1: Write integration test**

```rust
#[test]
fn test_all_core_skills_load() {
    let mut registry = SkillRegistry::new();
    let count = registry.load_from_dir(Path::new("skills/core")).unwrap();
    assert_eq!(count, 7, "Expected 7 core skills");
    let names: Vec<&str> = registry.list().iter().map(|s| s.skill.name.as_str()).collect();
    assert!(names.contains(&"compliance-auditor"));
    assert!(names.contains(&"secops-monitor"));
    assert!(names.contains(&"infra-watcher"));
    assert!(names.contains(&"cost-optimizer"));
    assert!(names.contains(&"backup-guardian"));
    assert!(names.contains(&"researcher"));
    assert!(names.contains(&"browser-automator"));
}
```

- [ ] **Step 2: Run test**

Run: `cargo test -p agentos-skills -- test_all_core_skills_load`

- [ ] **Step 3: Commit**

```bash
git add skills/core/ crates/agentos-skills/
git commit -m "feat(skills): add all 7 core pre-built agents"
```

## Verification

```bash
cargo build --workspace
cargo test -p agentos-skills
cargo clippy --workspace -- -D warnings
ls skills/core/*/SKILL.toml | wc -l  # Should output 7
```
