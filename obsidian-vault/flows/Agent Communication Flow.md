---
title: Agent Communication Flow
tags: [flow, agents, messaging]
---

# Agent Communication Flow

Agents communicate through the Agent Message Bus, enabling collaboration, task delegation, and information sharing.

## Message Targets

| Target | Description |
|---|---|
| `Direct(AgentID)` | Send to a specific agent by ID |
| `DirectByName(String)` | Send to an agent by name |
| `Group(GroupID)` | Send to all agents in a group |
| `Broadcast` | Send to all connected agents |

## Direct Messaging

```
Agent A                    Message Bus                   Agent B
   │                           │                            │
   │── agent-message tool ────►│                            │
   │   (to: "agent-b")        │                            │
   │                           │── push to B's inbox ──────►│
   │                           │                            │
   │                           │◄── reply (optional) ───────│
   │◄── receive reply ────────│                            │
```

### CLI Usage
```bash
# Send a direct message
agentctl agent message analyst "Here is the parsed data: {...}"

# View agent's inbox
agentctl agent messages analyst --last 10
```

### Tool Usage (from within a task)
The `agent-message` built-in tool allows agents to message each other during task execution:
```json
{
  "to": "coder",
  "content": "Please implement the parser based on this spec: ..."
}
```

## Group Messaging

```bash
# Create a group
agentctl agent group create --name research-team --members analyst,researcher,writer

# Broadcast to group
agentctl agent broadcast --group research-team "New data available for analysis"
```

All members receive the message in their inboxes.

## Task Delegation

The `task-delegate` tool allows an agent to spawn sub-tasks for other agents:

```
Agent A (running task)
    │
    ├── task-delegate tool
    │   { "agent": "coder", "task": "Write the parser" }
    │
    ▼
Kernel creates sub-task
    │
    ├── parent_task: A's task ID
    ├── Routes to Agent B (coder)
    └── Returns result to A's context
```

## Implementation Details

- Each agent has an `mpsc` channel inbox
- Messages stored in history for retrieval
- Messages include optional `reply_to` field for threading
- All messaging requires `agent.message:w` permission
- Broadcast requires `agent.broadcast:w` permission
