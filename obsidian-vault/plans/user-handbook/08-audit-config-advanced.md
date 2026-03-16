---
title: Handbook Audit Config Advanced
tags:
  - docs
  - kernel
  - v3
  - plan
date: 2026-03-13
status: planned
effort: 4h
priority: high
---

# Handbook Audit Config Advanced

> Write four chapters: Audit Log, LLM Configuration, Configuration Reference, and Advanced Operations (HAL, resource arbitration, snapshots/rollback, identity management, escalation management).

---

## Why This Subtask
These chapters cover operational concerns that advanced users and administrators need. The audit system has Merkle chain verification, export, and snapshot management that are undocumented. LLM configuration across 5 providers needs a unified reference. The full configuration reference needs every key from both config files documented. Advanced operations (HAL, resource locks, snapshots) have zero user documentation.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Audit docs | 2 lines in CLI ref | Full chapter: event types, query, verify, export, snapshots, rollback |
| LLM config | Brief in getting-started | Full chapter: all 5 providers, API key handling, endpoint resolution, environment variables |
| Config reference | Partial in config guide | Complete reference: every key in `default.toml` and `production.toml` |
| HAL docs | None | Overview section with available monitoring |
| Resource arbitration | None | Full section: shared/exclusive locks, contention, release |
| Snapshots | None | Full section: list, rollback, auto-snapshot behavior, expiry |

---

## What to Do

### 1. Write `14-Audit Log.md`

Read these source files for ground truth:
- `crates/agentos-audit/src/log.rs` -- `AuditLog`, `AuditEntry`, `AuditEventType` (all 50+ event types), `AuditSeverity`, `ChainVerification`
- `crates/agentos-cli/src/commands/audit.rs` -- audit CLI commands (logs, verify, snapshots, export, rollback)
- `crates/agentos-kernel/src/snapshot.rs` -- snapshot management

The chapter must include:

**Section: What is the Audit Log**
- Append-only SQLite database
- Every significant action recorded (50+ event types)
- Merkle hash chain for tamper detection

**Section: Querying Audit Logs**
- `agentctl audit logs --last <N>`
- Output format: TIMESTAMP, EVENT TYPE, SEVERITY, DETAILS

**Section: Audit Event Types**
- Complete categorized table of all `AuditEventType` variants (from `log.rs`)
- Grouped by category: Task, Intent, Permission, Tool, Agent, LLM, Secret, System, Schedule, Background, Budget, Risk, Snapshot, Cost, Event

**Section: Severity Levels**
- `Info`, `Warn`, `Error`, `Security` -- when each is used

**Section: Merkle Chain Verification**
- `agentctl audit verify [--from <seq>]`
- What it checks: SHA-256 hash chain integrity
- Output: VALID/INVALID with entry count and first invalid sequence

**Section: Exporting the Audit Chain**
- `agentctl audit export [--limit N] [--output path]`
- JSONL format

**Section: Context Snapshots**
- `agentctl audit snapshots --task <task-id>` -- list snapshots for a task
- Auto-snapshot behavior: taken before write operations and budget exhaustion
- Snapshot expiry: cleaned up after 72 hours

**Section: Rolling Back**
- `agentctl audit rollback --task <task-id> [--snapshot <ref>]`
- Restores task context to a saved snapshot state

**Section: AuditEntry Structure**
- Fields: timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity, reversible, rollback_ref

### 2. Write `15-LLM Configuration.md`

Read these source files for ground truth:
- `crates/agentos-llm/src/ollama.rs` -- Ollama adapter
- `crates/agentos-llm/src/openai.rs` -- OpenAI adapter
- `crates/agentos-llm/src/anthropic.rs` -- Anthropic adapter
- `crates/agentos-llm/src/gemini.rs` -- Gemini adapter
- `crates/agentos-llm/src/custom.rs` -- Custom adapter
- `crates/agentos-llm/src/traits.rs` -- `LLMCore` trait
- `crates/agentos-llm/src/types.rs` -- `ModelCapabilities`, `ModelPricing`, `InferenceCost`
- `config/default.toml` -- `[ollama]`, `[llm]` sections
- `docs/guide/07-configuration.md` -- existing LLM endpoint resolution docs

The chapter must include:

**Section: Supported Providers**
- Ollama (local, self-hosted)
- OpenAI (GPT-4o, GPT-3.5, etc.)
- Anthropic (Claude models)
- Gemini (Google AI)
- Custom (any OpenAI-compatible endpoint)

**Section: Provider Configuration**
For each provider:
- Config key in `default.toml`
- Environment variable override
- CLI `--base-url` override
- API key handling (vault-stored)
- Endpoint resolution precedence

**Section: Connecting Agents by Provider**
- Ollama: `agentctl agent connect --provider ollama --model llama3.2 --name local`
- OpenAI: `agentctl agent connect --provider openai --model gpt-4o --name researcher`
- Anthropic: `agentctl agent connect --provider anthropic --model claude-sonnet-4-20250514 --name coder`
- Gemini: `agentctl agent connect --provider gemini --model gemini-1.5-pro --name writer`
- Custom: `agentctl agent connect --provider custom --model my-model --name custom --base-url http://host/v1`

**Section: Environment Variables**
- `AGENTOS_OLLAMA_HOST` -- override Ollama host
- `AGENTOS_LLM_URL` -- override custom provider URL
- `AGENTOS_OPENAI_BASE_URL` -- override OpenAI base URL
- `RUST_LOG` -- set log level

**Section: LLMCore Trait**
- Brief explanation of the adapter interface (not for users to implement, but to understand)
- `infer()`, `infer_stream()`, `health_check()`, `capabilities()`, `provider_name()`, `model_name()`

**Section: Model Capabilities**
- Context window size
- Streaming support
- Tool call support

### 3. Write `16-Configuration Reference.md`

Read these source files for ground truth:
- `config/default.toml` -- all development config keys
- `config/production.toml` -- all production config keys
- `crates/agentos-kernel/src/config.rs` -- config loading and struct definition

The chapter must include a complete reference table for every config section:

- `[kernel]` -- `max_concurrent_tasks`, `default_task_timeout_secs`, `context_window_max_entries`, `context_window_token_budget`, `health_port` (production)
- `[secrets]` -- `vault_path`
- `[audit]` -- `log_path`
- `[tools]` -- `core_tools_dir`, `user_tools_dir`, `data_dir`
- `[bus]` -- `socket_path`
- `[ollama]` -- `host`, `default_model`
- `[llm]` -- `custom_base_url`, `openai_base_url`, `anthropic_base_url`, `gemini_base_url`
- `[memory]` -- `model_cache_dir`
- `[memory.extraction]` -- `enabled`, `conflict_threshold`, `max_facts_per_result`, `min_result_length`
- `[memory.consolidation]` -- `enabled`, `min_pattern_occurrences`, `task_completions_trigger`, `time_trigger_hours`, `max_episodes_per_cycle`
- `[context_budget]` -- `total_tokens`, `reserve_pct`, `system_pct`, `tools_pct`, `knowledge_pct`, `history_pct`, `task_pct`

For each key: name, type, default value, description, valid values/range.

Include the full `default.toml` and `production.toml` contents.

### 4. Write `18-Advanced Operations.md`

Read these source files for ground truth:
- `crates/agentos-hal/src/lib.rs` -- HAL module exports
- `crates/agentos-cli/src/commands/resource.rs` -- resource lock CLI commands
- `crates/agentos-kernel/src/resource_arbiter.rs` -- FIFO lock management, deadlock detection
- `crates/agentos-cli/src/commands/snapshot.rs` -- snapshot CLI commands
- `crates/agentos-kernel/src/snapshot.rs` -- snapshot management
- `crates/agentos-cli/src/commands/identity.rs` -- identity CLI commands
- `crates/agentos-cli/src/commands/escalation.rs` -- escalation CLI commands

The chapter must include:

**Section: Hardware Abstraction Layer (HAL)**
- Overview: system, process, network, GPU monitoring
- Current status: registry with quarantine/approve/deny workflow
- Related tools: `sys-monitor`, `hardware-info`, `process-manager`, `network-monitor`
- Note: HAL is under active development (Spec #9)

**Section: Resource Arbitration**
- Shared/exclusive locks for multi-agent resource access
- FIFO waiters with deadlock detection (DFS cycle check)
- TTL sweep for expired locks
- CLI: `agentctl resource list`, `resource release`, `resource contention`, `resource release-all`

**Section: Snapshots and Rollback**
- Auto-snapshots: taken before write operations and on budget exhaustion
- Manual listing: `agentctl snapshot list --task <id>`
- Manual rollback: `agentctl snapshot rollback --task <id> [--snapshot <ref>]`
- Snapshot expiry: swept every 10 minutes, deleted after 72 hours
- Also accessible via `agentctl audit snapshots/rollback`

**Section: Escalation Management**
- When escalations are created (Level 3-4 risk actions)
- CLI: `agentctl escalation list [--all]`, `escalation get <id>`, `escalation resolve <id> --decision "..."`
- Auto-expiry after 5 minutes
- Resolution resumes paused task

**Section: Identity Management**
- Ed25519 keypair per agent
- `agentctl identity show --agent <name>` -- view public key
- `agentctl identity revoke --agent <name>` -- revoke identity and all permissions

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/14-Audit Log.md` | Create new |
| `obsidian-vault/reference/handbook/15-LLM Configuration.md` | Create new |
| `obsidian-vault/reference/handbook/16-Configuration Reference.md` | Create new |
| `obsidian-vault/reference/handbook/18-Advanced Operations.md` | Create new |

---

## Prerequisites
[[02-cli-reference]] should be complete for cross-referencing CLI commands.

---

## Test Plan
- All four files exist
- Audit chapter lists all 50+ event types from `AuditEventType`
- LLM chapter covers all 5 providers with connection examples
- Config chapter documents every key from both `default.toml` and `production.toml`
- Advanced Operations covers resource, snapshot, escalation, identity, and HAL

---

## Verification
```bash
test -f obsidian-vault/reference/handbook/14-Audit\ Log.md
test -f obsidian-vault/reference/handbook/15-LLM\ Configuration.md
test -f obsidian-vault/reference/handbook/16-Configuration\ Reference.md
test -f obsidian-vault/reference/handbook/18-Advanced\ Operations.md

# Audit chapter has event types
grep -c "EventType\|TaskCreated\|AgentConnected" \
  obsidian-vault/reference/handbook/14-Audit\ Log.md
# Should be >= 10

# Config chapter has all sections
for section in kernel secrets audit tools bus ollama llm memory context_budget; do
  grep -q "\[$section\]" obsidian-vault/reference/handbook/16-Configuration\ Reference.md || echo "MISSING: [$section]"
done
```
