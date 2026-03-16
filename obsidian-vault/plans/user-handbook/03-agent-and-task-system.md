---
title: Handbook Agent and Task System
tags:
  - docs
  - kernel
  - v3
  - plan
date: 2026-03-13
status: complete
effort: 3h
priority: high
---

# Handbook Agent and Task System

> Write the Agent Management and Task System chapters covering agent lifecycle, messaging, groups, task creation, routing, lifecycle states, escalation pausing, and monitoring.

---

## Why This Subtask
Agents and tasks are the two most fundamental concepts a user interacts with. The existing documentation covers basic `agent connect/list/disconnect` and `task run/list/logs/cancel`, but is missing agent messaging (direct, broadcast, groups), task routing strategies, task lifecycle states (queued, running, paused, completed, failed), escalation-triggered pausing, and background/scheduled tasks.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Agent lifecycle | Basic connect/list/disconnect | Full lifecycle: connect, configure permissions, assign roles, message, group, broadcast, disconnect |
| Agent messaging | Mentioned but not fully documented | Dedicated section: direct message, task delegation, broadcast, groups, security rules |
| Task lifecycle | Basic run/list/logs/cancel | Full lifecycle diagram: queued -> running -> (paused/escalated) -> completed/failed, with routing strategies |
| Task routing | Brief mention | Full section: 4 strategies (capability-first, cost-first, latency-first, round-robin), routing rules (regex-based) |
| Background tasks | Basic bg commands | Full section: detached tasks, log following, kill |
| Scheduled tasks | Basic schedule commands | Full section: cron expressions, pause/resume, permissions |

---

## What to Do

### 1. Write `05-Agent Management.md`

Read these source files for ground truth:
- `crates/agentos-cli/src/commands/agent.rs` -- all 7 subcommands (`connect`, `list`, `disconnect`, `message`, `messages`, `group create`, `broadcast`)
- `crates/agentos-types/src/agent.rs` -- `AgentProfile`, `AgentStatus`, `LLMProvider` types
- `crates/agentos-kernel/src/agent_registry.rs` -- agent registration logic
- `crates/agentos-kernel/src/agent_message_bus.rs` -- message bus internals
- `crates/agentos-types/src/agent_message.rs` -- `AgentMessage`, `MessageContent` types

The chapter must include:
- **What is an Agent** -- an LLM connected to the kernel with a name, provider, model, and permissions
- **Agent lifecycle diagram** -- connect -> online -> (execute tasks) -> disconnect
- **Connecting agents** -- all 5 providers (Ollama, OpenAI, Anthropic, Gemini, Custom) with examples and API key handling
- **Listing agents** -- output format (NAME, PROVIDER, MODEL columns)
- **Disconnecting agents** -- by name
- **Agent messaging** -- direct messages between agents
  - `agentctl agent message --from sender --to recipient "content"`
  - Security: requires `agent.message:x` permission to send, `agent.message:r` to receive
- **Viewing messages** -- `agentctl agent messages <agent> --last N`
- **Agent groups** -- creating groups, broadcasting to groups
  - `agentctl agent group create <name> --members "a,b,c"`
  - `agentctl agent broadcast --from sender <group> "content"`
- **Identity** -- Ed25519 cryptographic identity per agent
  - `agentctl identity show --agent <name>`
  - `agentctl identity revoke --agent <name>`
- **Permissions and roles** -- brief overview linking to [[08-Security Model]]

### 2. Write `06-Task System.md`

Read these source files for ground truth:
- `crates/agentos-cli/src/commands/task.rs` -- task CLI commands
- `crates/agentos-types/src/task.rs` -- `AgentTask`, `TaskState` types
- `crates/agentos-kernel/src/scheduler.rs` -- `TaskScheduler`, `TaskDependencyGraph`
- `crates/agentos-kernel/src/router.rs` -- routing strategies
- `crates/agentos-kernel/src/task_executor.rs` -- task execution logic, tool call loop, escalation pausing
- `crates/agentos-kernel/src/risk_classifier.rs` -- `ActionRiskLevel` taxonomy
- `crates/agentos-cli/src/commands/bg.rs` -- background task CLI
- `crates/agentos-cli/src/commands/schedule.rs` -- scheduled task CLI

The chapter must include:
- **What is a Task** -- a prompt sent to an agent for execution, with a unique TaskID, context window, and capability token
- **Task lifecycle** -- state diagram: Queued -> Running -> (Paused for escalation | Completed | Failed | Cancelled | TimedOut)
- **Creating tasks** -- `agentctl task run [--agent name] "prompt"`, auto-routing if no agent specified
- **Task routing** -- 4 strategies table with priority order for each
- **Task execution flow** -- numbered steps: intent parsing, capability check, tool calls, result injection, context compilation
- **Tool call loop** -- how the kernel detects tool calls in LLM output, validates permissions, executes tools, re-injects results
- **Paused tasks** -- when a task hits a high-risk action (Level 3-4), the kernel pauses it and creates an escalation
- **Listing tasks** -- output format (TASK ID, STATE, AGENT, PROMPT columns)
- **Task logs** -- `agentctl task logs <task-id>`
- **Cancelling tasks** -- `agentctl task cancel <task-id>`
- **Background tasks** -- `agentctl bg run/list/logs/kill` with examples
- **Scheduled tasks** -- `agentctl schedule create/list/pause/resume/delete` with cron expression examples
- **Task timeouts** -- configurable via `[kernel].default_task_timeout_secs`
- **Concurrent task limits** -- configurable via `[kernel].max_concurrent_tasks`

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/05-Agent Management.md` | Create new |
| `obsidian-vault/reference/handbook/06-Task System.md` | Create new |

---

## Prerequisites
[[02-cli-reference]] should be complete so the CLI reference can be cross-referenced.

---

## Test Plan
- Both files exist in `obsidian-vault/reference/handbook/`
- Agent Management covers all 7 agent subcommands plus identity commands
- Task System covers task lifecycle states, all 4 routing strategies, background tasks, and scheduled tasks
- Both chapters include practical examples with `agentctl` commands
- Message bus security rules are documented (permission requirements)

---

## Verification
```bash
test -f obsidian-vault/reference/handbook/05-Agent\ Management.md
test -f obsidian-vault/reference/handbook/06-Task\ System.md

# Agent chapter covers key subcommands
grep -c "agentctl agent" obsidian-vault/reference/handbook/05-Agent\ Management.md
# Should be >= 7

# Task chapter covers all states
grep -c "TaskState\|Queued\|Running\|Paused\|Completed\|Failed" obsidian-vault/reference/handbook/06-Task\ System.md
# Should be >= 5
```
