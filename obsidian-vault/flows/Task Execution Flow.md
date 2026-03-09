---
title: Task Execution Flow
tags: [flow, task]
---

# Task Execution Flow

The complete journey of a task from CLI submission to completion.

## Overview

```
CLI ─► Bus ─► Kernel ─► Scheduler ─► Router ─► LLM Loop ─► Complete
```

## Detailed Steps

### 1. Task Submission
```bash
agentctl task run --agent analyst "Summarize the quarterly report"
```
CLI sends `KernelCommand::RunTask { agent_name, prompt }` via [[Message Bus|bus]].

### 2. Task Creation
Kernel creates `AgentTask`:
- Generates `TaskID` (UUID v4)
- Sets state to `Queued`
- Assigns priority (default or specified)
- Sets timeout from config

### 3. Agent Routing
If `--agent` specified:
- Look up agent by name in registry
- Verify agent is `Online`

If auto-routing:
- Apply routing rules (regex on prompt)
- Fall back to routing strategy (CapabilityFirst, CostFirst, etc.)

### 4. Token Issuance
[[Capability and Permissions|Capability Engine]] mints a token:
- Scoped to this task + agent
- Contains agent's effective permissions
- Lists allowed tools and intent types
- HMAC-SHA256 signed
- Time-limited (TTL-based)

### 5. Context Setup
[[Kernel Deep Dive#Context Manager|Context Manager]] creates a context window:
- System prompt (kernel instructions)
- User prompt (the task text)
- Max entries from config (default 100)

### 6. LLM Inference Loop

```
┌──────────────────────────────────────────────┐
│                                              │
│  Context Window ──► LLM.infer() ──► Response │
│       ▲                                │     │
│       │                                ▼     │
│  Push Result    ◄── Tool Execute ◄── Parse   │
│                                              │
│  Loop until LLM signals completion           │
└──────────────────────────────────────────────┘
```

**Each iteration:**
1. Send context to LLM adapter → get `InferenceResult`
2. Parse response for tool calls
3. If no tool calls → task is done
4. For each tool call:
   a. Validate capability token
   b. Check permissions
   c. Execute tool via `ToolRunner`
   d. Push result to context
5. Add assistant response to context
6. Repeat

### 7. Task State Transitions

```
Queued ──► Running ──► Complete
                   ──► Failed
                   ──► Waiting (tool execution)
                         │
                         └──► Running (result received)

Any state ──► Cancelled (user cancellation)
```

### 8. Completion
- Task state set to `Complete` or `Failed`
- Audit entry recorded
- Context window cleaned up
- Agent status returned to `Idle`

## Error Handling

| Error | Handling |
|---|---|
| LLM timeout | Task marked `Failed`, logged |
| Tool execution error | Error pushed to context, LLM can retry |
| Permission denied | Error pushed to context |
| Token expired | New token issued if task still valid |
| Agent disconnected | Task cancelled |
