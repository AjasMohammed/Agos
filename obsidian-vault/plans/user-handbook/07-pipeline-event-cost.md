---
title: Handbook Pipeline Event Cost
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

# Handbook Pipeline Event Cost

> Write three chapters: Pipeline and Workflows, Event System, and Cost Tracking -- covering multi-step orchestration, event subscriptions/triggers, and LLM cost monitoring/budgets.

---

## Why This Subtask
These three systems are core V3 features with zero user-facing documentation. Pipelines enable multi-step workflows; the event system enables reactive agent triggering; cost tracking enables budget enforcement and cost attribution. Users need to understand how to define pipelines in YAML, subscribe agents to events, and monitor/control LLM spending.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Pipeline docs | Internal reference only (`obsidian-vault/reference/Pipeline System.md`) | User-facing chapter with YAML format, step types, failure handling, variable passing, examples |
| Event system docs | None | Full chapter: event types, subscriptions, filters, throttling, triggered tasks, history |
| Cost tracking docs | None | Full chapter: per-agent reports, budget enforcement, model downgrade, retrieval efficiency |

---

## What to Do

### 1. Write `11-Pipeline and Workflows.md`

Read these source files for ground truth:
- `crates/agentos-pipeline/src/definition.rs` -- `PipelineDefinition`, `PipelineStep`, `StepAction`, `OnFailure` types
- `crates/agentos-pipeline/src/engine.rs` -- `PipelineEngine`, execution logic
- `crates/agentos-pipeline/src/store.rs` -- `PipelineStore`, `PipelineSummary`
- `crates/agentos-pipeline/src/types.rs` -- `PipelineRun`, `PipelineRunStatus`, `StepResult`, `StepStatus`
- `crates/agentos-cli/src/commands/pipeline.rs` -- pipeline CLI commands
- `crates/agentos-kernel/src/commands/pipeline.rs` -- kernel pipeline handler

The chapter must include:

**Section: What is a Pipeline**
- Multi-step workflow composed of sequential or parallel steps
- Each step is either an agent task or a direct tool invocation
- Steps can depend on other steps, pass variables, retry on failure

**Section: Pipeline YAML Format**
Full annotated example:
```yaml
name: data-analysis
version: "1.0"
description: "Fetch, parse, and summarize a data file"
permissions:
  - "fs.user_data:r"
  - "network.outbound:x"
max_cost_usd: 0.50
max_wall_time_minutes: 10
output: summary

steps:
  - id: fetch
    agent: researcher
    task: "Download the latest data from https://example.com/data.csv"
    output_var: raw_data
    timeout_minutes: 2
    retry_on_failure: 2
    on_failure: fail

  - id: parse
    tool: data-parser
    input: { "data": "{{raw_data}}", "format": "csv" }
    output_var: parsed
    depends_on: [fetch]

  - id: analyze
    agent: analyst
    task: "Analyze this data and produce a summary: {{parsed}}"
    output_var: summary
    depends_on: [parse]
    on_failure: use_default
    default_value: "Analysis could not be completed"
```

Explain every field:
- `name`, `version`, `description` -- metadata
- `permissions` -- required permissions for all steps
- `max_cost_usd` -- total budget cap
- `max_wall_time_minutes` -- wall-clock timeout
- `output` -- which `output_var` is the final result
- Per-step fields: `id`, `agent`/`tool`, `task`/`input`, `output_var`, `depends_on`, `timeout_minutes`, `retry_on_failure`, `on_failure` (fail/skip/use_default), `default_value`

**Section: Step Types**
- Agent step: `agent` + `task` -- sends a prompt to an agent
- Tool step: `tool` + `input` -- directly invokes a tool with JSON input
- Variable interpolation: `{{var_name}}` substitutes previous step outputs

**Section: Failure Handling**
- `fail` (default) -- stop the entire pipeline
- `skip` -- mark step as skipped, continue to next
- `use_default` -- use `default_value` as step output, continue

**Section: CLI Commands**
- `agentctl pipeline install <path>` -- install from YAML file
- `agentctl pipeline list` -- list installed pipelines
- `agentctl pipeline run <name> --input "..." [--detach]` -- run a pipeline
- `agentctl pipeline status <name> --run-id <id>` -- check run status
- `agentctl pipeline logs <name> --run-id <id> --step <step-id>` -- view step logs
- `agentctl pipeline remove <name>` -- remove a pipeline

### 2. Write `12-Event System.md`

Read these source files for ground truth:
- `crates/agentos-kernel/src/event_bus.rs` -- event bus implementation
- `crates/agentos-kernel/src/event_dispatch.rs` -- event dispatch and subscription matching
- `crates/agentos-kernel/src/commands/event.rs` -- kernel event command handlers
- `crates/agentos-cli/src/commands/event.rs` -- event CLI commands
- `crates/agentos-audit/src/log.rs` -- `AuditEventType` enum for event types

The chapter must include:

**Section: What is the Event System**
- Event bus for kernel-emitted events (agent connected, task completed, cost threshold hit, etc.)
- Agents can subscribe to event types and be triggered to execute tasks when events occur

**Section: Event Types**
Full table of all event types from `AuditEventType`:
- Task lifecycle: `TaskCreated`, `TaskStateChanged`, `TaskCompleted`, `TaskFailed`, `TaskTimeout`
- Intent: `IntentReceived`, `IntentRouted`, `IntentCompleted`, `IntentFailed`
- Permission: `PermissionGranted`, `PermissionRevoked`, `PermissionDenied`, `TokenIssued`, `TokenExpired`
- Tool: `ToolInstalled`, `ToolRemoved`, `ToolExecutionStarted`, `ToolExecutionCompleted`, `ToolExecutionFailed`
- Agent: `AgentConnected`, `AgentDisconnected`
- LLM: `LLMInferenceStarted`, `LLMInferenceCompleted`, `LLMInferenceError`
- Secret: `SecretCreated`, `SecretAccessed`, `SecretRevoked`, `SecretRotated`
- System: `KernelStarted`, `KernelShutdown`, `KernelSubsystemRestarted`
- Schedule: `ScheduledJobCreated`, `ScheduledJobFired`, `ScheduledJobPaused`, `ScheduledJobResumed`, `ScheduledJobDeleted`
- Background: `BackgroundTaskStarted`, `BackgroundTaskCompleted`, `BackgroundTaskFailed`, `BackgroundTaskKilled`
- Budget: `BudgetWarning`, `BudgetExceeded`
- Risk: `RiskEscalation`, `ActionForbidden`
- Snapshot: `SnapshotTaken`, `SnapshotRestored`, `SnapshotExpired`
- Cost: `CostAttribution`
- Event: `EventEmitted`, `EventSubscriptionCreated`, `EventSubscriptionRemoved`, `EventDelivered`, `EventThrottled`, `EventFilterRejected`, `EventLoopDetected`, `EventTriggeredTask`, `EventTriggerFailed`

**Section: Subscribing to Events**
- `agentctl event subscribe --agent <name> --event <type> [--filter "expr"] [--throttle "policy"] [--priority level]`
- Event filter: `all`, `category:<name>`, or exact event type
- Payload filter: expression syntax (e.g., `cpu_percent > 90 AND severity == Critical`)
- Throttle policies: `none`, `once_per:<duration>`, `max:<count>/<duration>`
- Priority levels: `critical`, `high`, `normal`, `low`

**Section: Managing Subscriptions**
- `agentctl event subscriptions list [--agent <name>]`
- `agentctl event subscriptions show --id <id>`
- `agentctl event subscriptions enable --id <id>`
- `agentctl event subscriptions disable --id <id>`
- `agentctl event unsubscribe <id>`

**Section: Event History**
- `agentctl event history --last <N>`
- Output format: TIMESTAMP, EVENT TYPE, SEVERITY, DEPTH

**Section: Event-Triggered Tasks**
- How a subscription can trigger an agent to execute a task when the event fires
- Loop detection: kernel prevents infinite event -> task -> event chains

### 3. Write `13-Cost Tracking.md`

Read these source files for ground truth:
- `crates/agentos-kernel/src/cost_tracker.rs` -- `CostTracker`, budget enforcement, `BudgetCheckResult`
- `crates/agentos-llm/src/types.rs` -- `InferenceCost`, `ModelPricing`, `calculate_inference_cost`, `default_pricing_table`
- `crates/agentos-cli/src/commands/cost.rs` -- cost CLI commands
- `crates/agentos-kernel/src/metrics.rs` -- retrieval metrics

The chapter must include:

**Section: Cost Tracking Overview**
- Per-agent tracking: tokens used, cost in USD, tool calls
- Per-inference cost calculation using pricing table
- Cost attribution logged to audit log

**Section: Viewing Cost Reports**
- `agentctl cost show` -- all agents
- `agentctl cost show --agent <name>` -- specific agent
- Output format: Agent, Tokens, Cost (USD), Tool Calls, percentages

**Section: Budget Enforcement**
- Per-agent budgets: token limit, USD limit, tool call limit
- `BudgetCheckResult`: `Allowed`, `ModelDowngradeRecommended`, `HardLimit`
- When budget is exceeded: task is paused, checkpoint taken, model downgrade attempted
- Budget warning vs budget exceeded events

**Section: Model Pricing**
- Default pricing table for common models
- How cost is calculated: `input_tokens * input_price + output_tokens * output_price`

**Section: Retrieval Efficiency Metrics**
- `agentctl cost retrieval` -- refresh/reuse efficiency
- Refresh vs reuse decisions, ratios

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/11-Pipeline and Workflows.md` | Create new |
| `obsidian-vault/reference/handbook/12-Event System.md` | Create new |
| `obsidian-vault/reference/handbook/13-Cost Tracking.md` | Create new |

---

## Prerequisites
[[02-cli-reference]] should be complete for cross-referencing CLI commands.

---

## Test Plan
- All three files exist
- Pipeline chapter has a complete annotated YAML example
- Event chapter lists all event types from `AuditEventType`
- Cost chapter documents the budget enforcement flow
- All CLI commands from the respective command files are documented

---

## Verification
```bash
test -f obsidian-vault/reference/handbook/11-Pipeline\ and\ Workflows.md
test -f obsidian-vault/reference/handbook/12-Event\ System.md
test -f obsidian-vault/reference/handbook/13-Cost\ Tracking.md

# Pipeline has YAML example
grep -c "yaml\|steps:\|depends_on" obsidian-vault/reference/handbook/11-Pipeline\ and\ Workflows.md
# Should be >= 3

# Event chapter has event types
grep -c "TaskCreated\|AgentConnected\|BudgetExceeded\|EventEmitted" \
  obsidian-vault/reference/handbook/12-Event\ System.md
# Should be >= 4
```
