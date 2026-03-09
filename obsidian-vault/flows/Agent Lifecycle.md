---
title: Agent Lifecycle
tags: [flow, agent]
---

# Agent Lifecycle

## States

```
               connect
    (none) ──────────────► Online
                              │
                    task run   │   idle timeout
                              ▼
                            Busy ◄────► Idle
                              │
                  disconnect  │
                              ▼
                           Offline
```

## 1. Connection

```bash
agentctl agent connect --provider ollama --model llama3.2 --name analyst
```

**What happens:**
1. CLI sends `KernelCommand::ConnectAgent`
2. Kernel validates LLM provider connectivity via `health_check()`
3. Creates `AgentProfile` with `Online` status
4. Registers in `AgentRegistry` with name index
5. Assigns `AgentID` (UUID v4)
6. Applies default "base" role permissions (`fs.user_data:rw`)
7. Creates inbox in `AgentMessageBus`
8. Returns `AgentID` to CLI

## 2. Task Execution

See [[Task Execution Flow]] for full details.

**Summary:**
1. Task queued in scheduler
2. Routed to agent (explicit or auto-routing)
3. Capability token issued (scoped to task)
4. Context window created with system prompt
5. LLM inference loop (prompt → response → tool calls → results → repeat)
6. Task completes or fails
7. Context cleaned up

## 3. Communication

See [[Agent Communication Flow]] for full details.

Agents can:
- Send **direct messages** to other agents
- Join **groups** for topic-based channels
- **Broadcast** to all agents in a group
- **Delegate tasks** to other agents

## 4. Permission Management

```bash
# Grant permissions
agentctl perm grant analyst "network.outbound:rx"

# Assign roles
agentctl role assign analyst researcher

# Time-limited grants
agentctl perm grant analyst "process.exec:x" --expires 3600
```

Effective permissions = base role + assigned roles + direct grants

## 5. Disconnection

```bash
agentctl agent disconnect analyst
```

**What happens:**
1. Agent status set to `Offline`
2. Current task (if any) cancelled
3. Agent removed from active LLM map
4. Inbox preserved (messages not lost)
5. Audit entry recorded
