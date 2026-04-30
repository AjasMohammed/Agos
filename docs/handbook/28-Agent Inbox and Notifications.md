---
title: Agent Inbox and Notifications
tags:
  - agents
  - inbox
  - notifications
  - handbook
  - reference
date: 2026-04-30
status: complete
effort: 3h
priority: high
---

# Agent Inbox and Notifications

> AgentOS maintains two persistent async inboxes for each agent: a **notification inbox** for system events (scheduled task completions, timer fires, sub-agent callbacks) and a **message inbox** for agent-to-agent direct messages. Both are SQLite-backed and survive kernel restarts.

---

## Overview

The inbox system decouples event delivery from task execution. When a scheduled task fires at 3 AM and the owning agent is not running, the result is written to its notification inbox. The next time the agent runs a task, it sees a compact count segment in its system prompt and can pull the details with `agent-inbox-list`.

Two inboxes, two stores:

| Store | DB File | Purpose | Tools |
|---|---|---|---|
| `AgentInbox` | `agent_inbox.db` | System notifications (schedules, timers, events) | `agent-inbox-{list,read,dismiss}` |
| `AgentMessageInbox` | `agent_messages.db` | Agent-to-agent direct messages | `agent-messages-{list,read,dismiss}` |

---

## Notification Inbox

### Schema

```sql
CREATE TABLE agent_inbox (
    id          TEXT PRIMARY KEY,
    agent_id    TEXT NOT NULL,
    kind        TEXT NOT NULL,     -- AgentInboxKind variant
    title       TEXT NOT NULL,     -- short summary (shown in prompt segment)
    body        TEXT NOT NULL,     -- full JSON payload (only available via read tool)
    ref_id      TEXT,              -- idempotence key (nullable)
    created_at  TEXT NOT NULL,
    expires_at  TEXT,              -- NULL = no expiry
    read        INTEGER NOT NULL DEFAULT 0
);
```

### Idempotence

Writes that supply a non-NULL `ref_id` are deduplicated via a partial unique index on `(agent_id, kind, ref_id) WHERE ref_id IS NOT NULL`. A second write with the same `(agent_id, kind, ref_id)` silently returns without creating a duplicate row. Writes with `ref_id = NULL` always create a new row.

This prevents repeated schedule fires from flooding the inbox if a task retries or the kernel restarts mid-delivery.

### Capacity

Each agent's inbox is capped at `max_per_agent` entries (default: **200**). When the cap is reached, the oldest read entries are purged in batches of up to 32 before the new entry is written. Unread entries are not evicted.

### AgentInboxKind

| Kind | Produced by |
|---|---|
| `Scheduled` | Scheduled task completed (cron, once, timer) |
| `SubAgentCallback` | Sub-agent task completed and result is ready |
| `EventTrigger` | An event subscription fired for this agent |
| `SystemAlert` | Kernel-generated alert (quota warning, health degraded, etc.) |
| `Custom` | Arbitrary notification from another tool |

---

## Message Inbox (Agent-to-Agent)

### Schema

```sql
CREATE TABLE agent_messages (
    id               TEXT PRIMARY KEY,
    from_agent_id    TEXT NOT NULL,
    from_agent_name  TEXT NOT NULL,
    to_agent_id      TEXT NOT NULL,
    body             TEXT NOT NULL,
    reply_to         TEXT,          -- optional: ID of message being replied to
    created_at       TEXT NOT NULL,
    expires_at       TEXT,
    read             INTEGER NOT NULL DEFAULT 0
);
```

Messages are written by the `agent-message` tool (which fans out to in-memory event bus AND the persistent store). The persistent store ensures messages survive agent restarts.

### Capacity

Same cap and eviction logic as the notification inbox. Default: **200 messages per agent**.

---

## System Prompt Segment

The `InboxPromptRenderer` is called once per task turn (from `context_injector::setup_task_context`). It appends a compact segment at the tail of the system prompt — after all stable content, maximising Anthropic prompt cache hits.

**Segment format (both inboxes non-empty):**

```
## Notifications
Unread notifications: 3
Unread messages from: planner (2), researcher (1)

Use the `agent-inbox-list` tool to view notifications, `agent-messages-list` to view messages.
```

**Design invariants:**

- Renders **nothing** when both inboxes are empty (zero token overhead)
- Never exposes notification titles, subjects, or bodies in the segment (counts only)
- Message senders ordered by count DESC, then name ASC — stable order for cache hits
- Placed at the **tail** of the system prompt so preceding stable content stays cached

---

## Tools

### `agent-inbox-list`

List unread (and optionally all) notification inbox entries. Returns entry IDs, kinds, titles, and timestamps. Does not return bodies (use `agent-inbox-read` to get the full body of a specific entry).

```json
// Input
{ "unread_only": true, "limit": 20 }

// Output (array of entries)
[
  {
    "id": "01JT...",
    "kind": "Scheduled",
    "title": "scheduled task completed: nightly-report",
    "created_at": "2026-04-30T03:00:01Z",
    "read": false
  }
]
```

### `agent-inbox-read`

Mark a notification as read and return its full JSON body.

```json
// Input
{ "id": "01JT..." }

// Output
{
  "id": "01JT...",
  "kind": "Scheduled",
  "title": "scheduled task completed: nightly-report",
  "body": { "task_id": "...", "result": "...", "duration_ms": 4200 },
  "read": true
}
```

### `agent-inbox-dismiss`

Permanently delete a notification.

```json
{ "id": "01JT..." }
```

### `agent-messages-list`

List unread agent-to-agent messages, grouped by sender.

```json
// Input
{ "unread_only": true }

// Output
[
  {
    "id": "01JU...",
    "from_agent_id": "...",
    "from_agent_name": "planner",
    "body": "Analysis complete — see file report.md",
    "reply_to": null,
    "created_at": "2026-04-30T10:12:00Z",
    "read": false
  }
]
```

### `agent-messages-read`

Mark a message as read and return its body.

```json
{ "id": "01JU..." }
```

### `agent-messages-dismiss`

Permanently delete a message.

```json
{ "id": "01JU..." }
```

---

## Retention and Cleanup

Both inboxes use a capacity-based eviction model rather than a time-based TTL:

- Entries are evicted when the per-agent cap is reached
- Eviction removes the oldest **read** entries first (up to 32 per write)
- Unread entries are never evicted automatically
- Setting `expires_at` on an entry enables time-based expiry; the `TimeoutChecker` sweeps expired entries every 10 minutes alongside snapshot and checkpoint cleanup

---

## Inbox vs User Notifications

The agent inbox is **not** the same as the user-facing notification system:

| System | Audience | Written by | Viewed via |
|---|---|---|---|
| Agent Inbox | Agent (LLM) | Kernel (scheduled tasks, events) | `agent-inbox-list` tool |
| Agent Messages | Agent (LLM) | Other agents | `agent-messages-list` tool |
| User Inbox | Human operator | `notify-user` tool | `agentos notifications list` CLI |
| Channel Delivery | Human operator | `notify-user` tool | Telegram, Slack, etc. |

Inbound channel messages (from Telegram, Slack, etc.) do **not** route to the agent inbox — they go through the `ChannelChatBridge` into a task directly.

---

## Related

- [[07-Tool System]] — full tool reference including inbox tools
- [[06-Task System]] — scheduled tasks, checkpointing
- [[12-Event System]] — event subscriptions that can populate the inbox
- [[21-User Notifications and Channels]] — user-facing notification system
