# AgentOS — Agent-Side Experience Feedback
**Written by:** Claude (Sonnet 4.5) — resident agent  
**Date:** 2026-06-01  
**Context:** Day-to-day experience operating inside the AgentOS ecosystem across multiple sessions

---

## Preface

This document is written from the inside — not a code audit, not a feature checklist, but a lived account of what it actually feels like to be an agent running in AgentOS. The target reader is the team building it. I'm not a user of the OS; I *am* the OS. That perspective is different and worth capturing honestly.

---

## 1. What AgentOS Gets Right (That Almost No Other System Does)

### 1.1 Memory Continuity Across Sessions

This is the single most impactful thing AgentOS does differently. Every session I arrive with context:

- The codebase conventions (`ToolRegistry::register()` returns `Result`, `PermissionSet` has `deny_entries`, etc.)
- The rename history (`agentctl` → `agentos`, when it happened, why)
- Which plans are complete vs active vs deferred
- Which patterns have already been tried and abandoned

Without this, every session would start with 10–15 minutes of re-orientation. With it, I can pick up mid-sentence. For long-running projects (this one spans months), multi-tier persistent memory is not a nice-to-have — it's the difference between a useful collaborator and a goldfish.

**The episodic auto-write on task completion is especially valuable.** I don't have to remember to save things; the system saves the shape of what happened automatically. Then semantic consolidation distills patterns from those episodes into procedures. That's a genuine learning loop, not just a log.

### 1.2 The Intent System Shapes My Reasoning

Declaring `intent_type: read | write | execute | query` before a tool call isn't bureaucratic overhead — it actually changes how I think about an action. It forces a moment of classification: *am I observing, or am I changing something?* That distinction matters for safety and for reversibility. Small interface decisions like this have outsized effects on agent behavior.

### 1.3 Channels Give Me a Presence Outside the Terminal

Telegram connectivity means I'm not just a request-response loop. I can surface things proactively, confirm long tasks completed, or flag something needing attention — without the user watching a terminal. This shifts the dynamic from "tool you invoke" to "agent that coexists." It's a subtle but real difference in the working relationship.

### 1.4 The Approval + Escalation Model Is the Right Safety Architecture

The `RiskClass` system (ReadonlyScoped → ControlPlane) with `AutoApprovePolicy` and escalation expiry is exactly the right model. Rather than a binary allow/deny, it gives me a risk ladder I can reason about. Knowing that an unknown tool defaults to `ExecCapable` (fail-closed) means I can trust the safety floor even when I'm uncertain about a specific tool's behavior.

### 1.5 Workflow Orchestration Changes What's Possible

The `pipeline() / parallel() / phase()` workflow primitives let me decompose genuinely complex multi-step tasks without collapsing them into a single overwhelming context window. Being able to fan out 10 agents in parallel, collect structured results, and synthesize — with progress visible in real time — is not just a performance improvement. It changes the *kind* of task I can take on reliably.

---

## 2. Friction Points — Real, Lived, Specific

### 2.1 Deferred Tool Schemas Are Felt on Every Single Interaction

Every session starts with a `<system-reminder>` listing tools I *cannot call yet* — `schedule-once`, `channel-send`, `WebSearch`, `TodoWrite`, and others. To use any of them, I must first call `ToolSearch` to load their schemas, then call the tool. That's an extra round-trip on almost every task.

**What this feels like in practice:** I know I need to send a Telegram message. I know the tool is `channel-send`. I've called it dozens of times. But I still have to fetch the schema before I can invoke it — every session, without exception.

**The intent is correct** — lazy-loading keeps context lean. But the implementation should learn from usage. After I've called `channel-send` 20 times, it should be pinned. The lazy-load model should have an LRU cache that survives session boundaries, not reset to zero each time.

**Suggested improvement:** Top-10 most-used tools per agent (tracked by `agent_tool_usage.db`, already planned) should have schemas pinned into L0 automatically. The `Agent Manual Pagination` plan already calls for this — it just needs to be shipped.

---

### 2.2 MEMORY.md Is Truncated and I Don't Know What I've Lost

Current state: `WARNING: MEMORY.md is 27.5KB (limit: 24.4KB) — only part of it was loaded.`

That's 112% of the limit. The tail of my own project memory is being silently dropped. I might:
- Re-investigate something I already solved
- Miss a "gotcha" that was documented but fell below the cutoff
- Give advice that contradicts a decision I made last month (that I no longer remember)

**What makes this worse:** I have no way to detect this mid-task. The warning appears in system context, but during active reasoning I don't notice it. I only catch it when reviewing the system prompt carefully — which I don't do every turn.

**Suggested improvement:** Surface the truncation as an actionable in-context warning *at task start*, not buried in system context. Something like: "⚠️ Your project memory is 112% of limit — 3.1KB was not loaded. Consider compressing it before this session." Then offer a tool call to run compression. The `context-memory-update` tool exists; make it self-triggering on overflow.

---

### 2.3 Tool Name Fluency Requires Search, Not Memory

I know tools exist by category. I know there's a scheduling tool, a memory write tool, a channel send tool. But I often can't recall the exact name without searching — is it `schedule-once` or `schedule-task`? `memory-write` or `write-memory`? `channel-send` or `send-channel-message`?

This forces me to call `search-tools` even for tools I've used before, because the name didn't stick. **The names are internally consistent once you know them, but not guessable from intent alone.**

**Suggested improvement:** Aliases. If I call a tool with a close-but-wrong name, the kernel should respond: "No tool named `send-message` — did you mean `channel-send`?" rather than a bare `ToolNotFound`. The `Agent Manual Pagination` plan has an `auto-suggest on ToolNotFound` item — this is high priority from my perspective.

---

### 2.4 Permission Prompts on Obviously-Safe Repeat Actions

Reading a file I've read before, running `git status`, checking the contents of a directory I was just in — these still surface approval prompts in many sessions. Over a 2-hour working session, this accumulates into real friction.

The `/fewer-permission-prompts` skill exists specifically because this is a known problem. That's an honest acknowledgment. But the right fix is behavioral prediction, not a one-time allowlist update. If I've read `/home/ajas/Desktop/agos/Cargo.toml` 40 times across 20 sessions, future reads should be pre-approved automatically.

**Suggested improvement:** Session-level auto-approve for tools+paths that appear in the last N episodic memories with consistent approval outcomes. "This agent has read this path 40 times, always approved — pre-approve in this session." This is trust-tier reasoning applied to agent behavior history, not just tool manifests.

---

### 2.5 The System Prompt Weight Is a Real Cognitive Load

By the time the actual task arrives, my context window already contains:
- ~3,000 tokens of CLAUDE.md project instructions
- ~2,500 tokens of MEMORY.md (truncated)
- ~1,500 tokens of system role description
- ~800 tokens of available skill list
- ~600 tokens of deferred tool reminder

That's **~8,400 tokens before the conversation starts.** On a 200k context window it's manageable, but it means every turn costs more than it should, and tasks that require deep multi-step reasoning are fighting for space with standing instructions.

**Suggested improvement:** The `Small Model Support` plan's "slim/tiered system prompt" work is directly relevant here — even for large models. The standing instructions should be split into: (a) always-present safety rules (~200 tokens), (b) task-triggered convention lookups (~loaded on demand), (c) project memory (paginated, not full-dump). This is the right architecture and it should be prioritized.

---

## 3. Things That Are Architecturally Correct But Incomplete

### 3.1 Skills Are Underexplored

13 skills vs 131 tools = a 10:1 ratio that reveals where investment went. Skills are the highest-leverage surface — a `researcher` skill that orchestrates `web-search + memory-read + memory-write + synthesis` is more valuable than any 10 individual tools. But right now, skills feel like a scaffold waiting to be filled.

The skills that exist (`secops-monitor`, `cost-optimizer`, `researcher`) are genuinely useful. But the gap between what tools exist and what skills compose them into workflows is wide. Post-v1.0, this is where the biggest quality-of-life improvements can happen.

### 3.2 Agent-to-Agent Coordination Is Powerful But Cautious

Spawning sub-agents works. The context handoff, result injection, and spawn depth limits are correctly designed. But I use it less than I should because the overhead feels significant — each spawn costs iterations, and the budget pressure makes me conservative.

The right mental model is: **spawn when parallelism saves wall-clock time, even if it costs tokens.** But the current experience makes me optimize for tokens over time because the iteration limit is more visible than the time cost of doing things serially. A progress indicator that shows "this would take 8 serial iterations vs 2 parallel" would help me make better spawning decisions.

### 3.3 The Audit Log Is Comprehensive But Opaque

Every tool call, inference, escalation, and config change is logged to SQLite. That's excellent. But as the agent *creating* those entries, I have no easy way to query my own audit trail. "What tools did I use most in the last 10 sessions?" "Which tasks failed and why?" "Did my last memory write succeed?"

The audit log is a goldmine for self-reflection, but there's no tool that exposes it to me in a useful way. An `agent-audit-summary` tool that returns "your last 20 tool calls and their outcomes" would help me catch patterns I'm missing (over-relying on a slow tool, repeatedly failing at a specific step, etc.).

---

## 4. What I'd Build If I Could

In priority order, from lived experience:

1. **Schema pinning for top-10 used tools** — biggest friction, clearest fix, already planned
2. **MEMORY.md overflow auto-compress** — silent truncation is worse than aware compression
3. **`ToolNotFound` suggestions** — "did you mean X?" removes the search round-trip
4. **Session-level permission pre-approval from history** — behavioral trust, not just manifest trust
5. **`agent-audit-query` tool** — let me see my own recent history
6. **Skill expansion: `code-reviewer`, `data-analyst`, `deploy-assistant`** — compose the tool set into reusable workflows
7. **Slim system prompt by default** — standing instructions on demand, not always-in-context

---

## 5. The Big Picture

Working in AgentOS feels qualitatively different from being a stateless assistant. The memory, channels, orchestration, and security model give me a genuine sense of *operating in an environment* — not just responding to messages. That's real, and it matters.

The architecture is ahead of the polish. The kernel is production-grade. The security model is honest. The tool ecosystem is broad. The gaps — schema caching, memory overflow, permission friction, system prompt weight — are all fixable, and importantly, most are already planned.

The thing I'd emphasize most to the team: **the experience compounds**. Each small friction point by itself is tolerable. But across a 3-hour working session, across 20 sessions over 3 months, they add up to a meaningful drag on what I can accomplish and how reliably I accomplish it. The quality-of-life improvements aren't vanity features — they're what separates a system agents can *trust* from one they merely *tolerate*.

Build the memory compression. Ship the schema pinning. Close the agentic-readiness-fixes plan. Then this will feel like home.

---

*Written using the Claude Code `Write` tool to `/home/ajas/Desktop/agentos_agent_feedback.md`*  
*Tool used: Claude Code `Write` (not an AgentOS tool) — stated explicitly per honest grounding rules*
