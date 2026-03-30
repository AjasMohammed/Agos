---
title: Cost Tracking
tags:
  - handbook
  - cost
  - budget
  - llm
  - v3
date: 2026-03-17
status: complete
---

# Cost Tracking

> The kernel tracks LLM token consumption and USD cost per agent in real time, enforces configurable budget limits, and reports usage via CLI. When a budget threshold is crossed, the kernel can warn, pause, downgrade to a cheaper model, or kill the agent's task.

---

## Cost Tracking Overview

The `CostTracker` (`crates/agentos-kernel/src/cost_tracker.rs`) maintains per-agent cost state in memory. For each registered agent it tracks:

- **Tokens used** — cumulative input + output tokens consumed in the current 24-hour budget period.
- **Cost in USD** — computed from the model's pricing table and accumulated as micro-USD (millionths of a dollar) to avoid floating-point precision loss.
- **Tool calls** — number of tool invocations in the current period.

Counters reset automatically every 24 hours using a lock-free `AtomicI64` period timestamp. A single `compare_exchange` ensures exactly one concurrent inference call performs the reset even under high parallelism.

Every completed inference call is also logged to the audit trail as a `CostAttribution` entry with structured JSON containing agent ID, model, token counts, and cost.

---

## Viewing Cost Reports

### All Agents

```bash
agentctl cost show
```

```
Agent                Tokens     Cost (USD)   Tool Calls    Tok %    Cost %   Call %
----------------------------------------------------------------------------------------
researcher             4200     0.000063           12      42.0%    12.6%    12.0%
analyst                2800     0.000042            8      28.0%     8.4%     8.0%
----------------------------------------------------------------------------------------
TOTAL                  7000     0.000105           20
```

Columns:

| Column | Description |
|--------|-------------|
| `Agent` | Agent display name. |
| `Tokens` | Total tokens (input + output) in the current budget period. |
| `Cost (USD)` | Accumulated cost in US dollars for the current period. |
| `Tool Calls` | Number of tool invocations in the current period. |
| `Tok %` | Tokens used as a percentage of `max_tokens_per_day`. |
| `Cost %` | Cost as a percentage of `max_cost_usd_per_day`. |
| `Call %` | Tool calls as a percentage of `max_tool_calls_per_day`. |

Percentages show `0.0%` when the corresponding limit is set to zero (unlimited).

### Specific Agent

```bash
agentctl cost show --agent <agent-name>
```

Same output, filtered to a single agent row.

---

## Budget Enforcement

Each agent is registered with an `AgentBudget` that defines limits and enforcement behavior.

### Budget Fields

| Field | Type | Description |
|-------|------|-------------|
| `max_tokens_per_day` | integer | Token limit per 24-hour period. `0` = unlimited. |
| `max_cost_usd_per_day` | float | USD cost limit per 24-hour period. `0.0` = unlimited. |
| `max_tool_calls_per_day` | integer | Tool call limit per 24-hour period. `0` = unlimited. |
| `warn_at_pct` | integer | Percentage at which a `BudgetWarning` alert is fired (e.g. `80` = warn at 80%). |
| `pause_at_pct` | integer | Percentage at which the task is paused or a model downgrade is attempted (e.g. `95`). |
| `on_hard_limit` | enum | Action taken when 100% of a limit is reached: `Suspend`, `NotifyOnly`, or `Kill`. |
| `downgrade_model` | struct (optional) | If set, pause-threshold events trigger a model downgrade instead of pausing. |
| `allowed_models` | list (optional) | If non-empty, only listed model names are permitted. Any other model is blocked before inference. |
| `max_wall_time_seconds` | integer | Wall-clock time limit per task. `0` = unlimited. |

### Budget Check Results

After every inference call and tool invocation, the tracker returns a `BudgetCheckResult`:

| Result | When | Kernel Action |
|--------|------|---------------|
| `Ok` | Usage is within all limits. | Proceed normally. |
| `Warning` | Usage crossed `warn_at_pct` on any resource. | Fire `BudgetWarning` event; continue execution. |
| `PauseRequired` | Usage crossed `pause_at_pct` and no downgrade model is configured. | Take checkpoint snapshot; suspend task. |
| `ModelDowngradeRecommended` | Usage crossed `pause_at_pct` and a `downgrade_model` is configured. | Switch subsequent LLM calls to the cheaper model; continue task. |
| `HardLimitExceeded` | Usage hit 100% of any limit. | Apply `on_hard_limit` action (`Suspend`, `NotifyOnly`, or `Kill`). |
| `ModelNotAllowed` | Agent attempted to use a model not in `allowed_models`. | Block the inference call before it is sent. |
| `WallTimeExceeded` | Task elapsed time exceeded `max_wall_time_seconds`. | Kill the task. |

### Enforcement Flow

```
Inference call received
  → validate_model() — check allowed_models allowlist
  → check_budget() — pre-flight read-only check against current counters
  → [send request to LLM provider]
  → record_inference() — accumulate tokens + cost, re-check limits
     → Warning       → emit BudgetWarning event, continue
     → ModelDowngradeRecommended → route next calls to cheaper model
     → PauseRequired → take snapshot, suspend task
     → HardLimitExceeded → apply on_hard_limit action
```

Tool calls follow the same pattern via `record_tool_call()`, checking only the `max_tool_calls_per_day` limit.

### Budget Alerts

Budget alerts are broadcast over an internal channel (subscribable by the kernel's alert handler). The `BudgetAlert` message carries the agent ID, agent name, and the `BudgetCheckResult`. The kernel translates alerts into `BudgetWarning` and `BudgetExceeded` audit events.

---

## Model Pricing

Cost is calculated per inference call using a built-in pricing table (`crates/agentos-llm/src/types.rs`).

### Cost Formula

```
cost = (prompt_tokens / 1000 * input_per_1k) + (completion_tokens / 1000 * output_per_1k)
```

### Default Pricing Table

Prices are in USD per 1,000 tokens (as of March 2026):

| Provider | Model | Input / 1K | Output / 1K |
|----------|-------|------------|-------------|
| `anthropic` | `claude-sonnet-4-6` | $0.003 | $0.015 |
| `anthropic` | `claude-opus-4-6` | $0.015 | $0.075 |
| `anthropic` | `claude-haiku-4-5` | $0.0008 | $0.004 |
| `openai` | `gpt-4o` | $0.0025 | $0.010 |
| `openai` | `gpt-4o-mini` | $0.00015 | $0.0006 |
| `gemini` | `gemini-2.0-flash` | $0.0001 | $0.0004 |
| `gemini` | `gemini-2.5-pro` | $0.00125 | $0.010 |
| `ollama` | `*` (all models) | $0.00 | $0.00 |

The pricing table is loaded at kernel startup. Unknown providers and models default to zero cost (the kernel does not block them; it just records no cost).

Ollama's wildcard `*` entry means all locally-hosted models are tracked at zero cost — accurate for self-hosted inference.

### Model Downgrade Example

An agent configured with:

```toml
[agents.researcher.budget]
max_tokens_per_day = 100_000
pause_at_pct = 90
downgrade_model = { model = "claude-haiku-4-5", provider = "anthropic" }
```

Will automatically switch from `claude-sonnet-4-6` to `claude-haiku-4-5` at 90,000 tokens consumed, instead of suspending. The task continues with the cheaper model for the remainder of the period.

---

## Retrieval Efficiency Metrics

The kernel's memory context assembly system tracks whether it refreshed (re-fetched) or reused (cached) memory retrievals. This informs token efficiency — unnecessary refreshes waste input tokens.

```bash
agentctl cost retrieval
```

```
Retrieval Refresh Efficiency
  Refresh decisions: 142
  Reuse decisions:   891
  Total decisions:   1033
  Refresh ratio:     13.75%
  Reuse ratio:       86.25%
```

A high reuse ratio indicates the memory retrieval cache is working well. A high refresh ratio suggests the context assembly is over-fetching — potentially indicating stale cache thresholds or high memory churn.

---

## Related

- [[12-Event System]] — `BudgetWarning` and `BudgetExceeded` events can be subscribed to
- [[11-Pipeline and Workflows]] — pipelines can set `max_cost_usd` caps
- [[09-Secrets and Vault]] — API keys for LLM providers are stored in the vault
