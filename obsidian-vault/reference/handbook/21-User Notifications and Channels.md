---
title: User Notifications and Channels
tags:
  - notifications
  - channels
  - handbook
  - reference
  - v3
date: 2026-03-25
status: complete
effort: 3h
priority: high
---

# User Notifications and Channels

> AgentOS agents can send messages directly to the operator via a structured notification inbox. Messages can be fire-and-forget notifications or interactive questions that pause the task until the user responds. Notifications are delivered to the CLI inbox, the web UI, and any registered external channels (Telegram, ntfy, email, webhook).

---

## Overview

The notification system gives agents a first-class way to communicate with the human operator without relying on task logs or LLM output. There are two interaction modes:

| Mode | Description | Task behaviour |
|------|-------------|----------------|
| **Notification** (`notify-user`) | Fire-and-forget message | Task continues immediately |
| **Question** (`ask-user`) | Blocking interactive question | Task pauses in `Waiting` state |

All messages are stored in the **user inbox** — a kernel-managed queue that persists across task runs. Messages are delivered to all registered channels simultaneously.

---

## Message Structure

Every message (`UserMessage`) carries:

| Field | Description |
|-------|-------------|
| `id` | Unique `NotificationID` (UUID) |
| `from` | Source: `Agent(AgentID)`, `Kernel`, or `System` |
| `task_id` | Optional — the task that generated the message |
| `kind` | Message kind (see below) |
| `priority` | `info`, `warning`, `urgent`, or `critical` |
| `subject` | Short summary ≤80 chars — used in CLI one-liners and email subjects |
| `body` | Full markdown body |
| `interaction` | Optional — present only for `Question` kind messages |
| `delivery_status` | Map of `channel_id → DeliveryStatus` (Pending / Delivered / Failed / Skipped) |
| `response` | Optional — populated when the user has responded to a Question |
| `read` | Whether the operator has read the message |
| `thread_id` | Optional — groups related messages from the same task |
| `created_at` | UTC timestamp |
| `expires_at` | Optional — after this time the message is auto-expired |

### Message Kinds

```
UserMessageKind::Notification
  — Simple informational message.

UserMessageKind::Question { question, options?, free_text_allowed }
  — Interactive question. Task pauses until user responds.
  — options: optional list of allowed string choices
  — free_text_allowed: whether typed responses are accepted (default: true)

UserMessageKind::TaskComplete { task_id, outcome, summary, duration_ms, iterations, cost_usd?, tool_calls }
  — Emitted automatically by the kernel when a task finishes.
  — outcome: success | failed | cancelled | timed_out

UserMessageKind::StatusUpdate { task_id, old_state, new_state, detail? }
  — Emitted when a task changes state.
```

### Priority Levels

| Priority | Use case |
|----------|----------|
| `info` | Routine status, completed operations |
| `warning` | Unexpected conditions that do not block the task |
| `urgent` | Requires prompt attention |
| `critical` | Security or data-loss risk — sent to all channels regardless of filters |

---

## The `notify-user` Tool

Fire-and-forget notification. The agent calls this tool and immediately continues execution.

| | |
|---|---|
| **Permission** | `user.notify:w` |
| **Task blocked?** | No |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `subject` | string | Yes | — | ≤80 chars; used as the email subject and CLI one-liner |
| `body` | string | Yes | — | Markdown-formatted message body |
| `priority` | string | No | `"info"` | `info`, `warning`, `urgent`, or `critical` |

**Example agent usage:**
```json
{
  "subject": "Analysis complete",
  "body": "Finished scanning 4,200 log lines. Found **3 anomalies** — see attached report.",
  "priority": "warning"
}
```

**Result:** Returns a `_kernel_action: "notify_user"` envelope. The kernel delivers the message to the inbox and all registered channels, then continues the task immediately.

---

## The `ask-user` Tool

Blocking interactive question. The task enters `Waiting` state until the operator responds (or the timeout fires).

| | |
|---|---|
| **Permission** | `user.interact:x` |
| **Task blocked?** | Yes — until responded or timed out |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `question` | string | Yes | — | The question to ask the user |
| `options` | array of strings | No | — | Optional list of allowed answer choices |
| `timeout_secs` | u64 | No | `300` | Seconds before `auto_action` fires (0 = no timeout) |
| `auto_action` | string | No | `"auto_denied"` | Text injected as the answer if timeout fires with no response |
| `priority` | string | No | `"info"` | Notification priority level |

**Example agent usage:**
```json
{
  "question": "Should I delete the 847 duplicate records from the users table?",
  "options": ["Yes, delete them", "No, skip", "Archive instead"],
  "timeout_secs": 600,
  "auto_action": "No, skip",
  "priority": "urgent"
}
```

**Result:** Returns a `_kernel_action: "ask_user"` envelope. The kernel:
1. Creates a `Question` notification in the inbox
2. Delivers it to all registered channels
3. Suspends the task in `Waiting` state
4. When the operator responds via `agentctl notifications respond <id> --response <text>` or the web UI, the response text is injected into the agent's context window and the task resumes

**Timeout behaviour:** If `timeout_secs` elapses with no user response, the kernel automatically injects `auto_action` as the answer and resumes the task. The default `"auto_denied"` is a safe fallback for destructive operations.

**Concurrent limits:** An agent can have at most 3 concurrent blocking questions (configurable via `InteractionRequest.max_concurrent`). Additional questions beyond this limit are delivered as non-blocking notifications instead.

---

## Responding to Questions

From the CLI:

```bash
# List all notifications (including unread questions)
agentctl notifications list --unread

# Show full question text and options
agentctl notifications read <notification-id>

# Submit your response
agentctl notifications respond <notification-id> --response "Yes, delete them"
```

From the web UI: navigate to the **Notifications** section in the sidebar and click **Reply** on any pending question.

---

## Notification Inbox CLI

### `notifications list`

List messages from the inbox. Defaults to the 50 most recent.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--unread` / `-u` | flag | `false` | Show only unread notifications |
| `--limit` / `-n` | u32 | `50` | Maximum number of notifications to show |

**Example:**
```bash
agentctl notifications list
agentctl notifications list --unread --limit 20
```

**Output columns:** `ID` (first 8 chars of UUID), `PRIORITY`, `READ`, `FROM`, `SUBJECT`

### `notifications read`

Show the full body of a notification and mark it as read.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | String | Full or partial notification UUID |

**Example:**
```bash
agentctl notifications read a3b2c1d0
```

For `Question` messages, also shows the question text, options (if any), and the current response (if answered).

### `notifications respond`

Submit a response to an interactive `Question` notification. Unblocks the waiting task.

| Flag | Type | Description |
|------|------|-------------|
| `--response` / `-r` | String | Your response text |
| `id` | String | Notification UUID (positional) |

**Example:**
```bash
agentctl notifications respond a3b2c1d0 --response "Yes, proceed"
```

### `notifications watch`

Poll for new notifications every 5 seconds. Press Ctrl-C to stop. Silently skips existing unread notifications on first poll so the terminal is not flooded.

*No flags.*

**Example:**
```bash
agentctl notifications watch
```

---

## Delivery Channels

Notifications are delivered in parallel to every registered channel. Built-in delivery targets:

| Channel ID | Description |
|------------|-------------|
| `cli` | Visible in `agentctl notifications list` and `watch` |
| `web` | Displayed in the web UI notification inbox |
| `telegram` | Delivered via Telegram Bot API |
| `ntfy` | Delivered via ntfy push notification |
| `email` | Sent as an email |
| `webhook` | HTTP POST to a custom URL |
| `desktop` | Native OS desktop notification (planned) |
| `slack` | Slack webhook or bot message (planned) |

Each channel independently tracks its `DeliveryStatus`:

| Status | Meaning |
|--------|---------|
| `Pending` | Not yet attempted |
| `Delivered { at }` | Successfully delivered with timestamp |
| `Failed { reason }` | Delivery error (network issue, invalid credential, etc.) |
| `Skipped` | Channel filtered out the message (e.g. priority below threshold) |

Delivery failures on one channel do not block delivery to others.

---

## Managing Channels

Channels are registered by the operator. Credentials (bot tokens, passwords) are stored in the vault and referenced by key name — never stored in plaintext in the channel record.

### `channel connect`

Register a new external delivery channel.

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--kind` / `-k` | String | Yes | — | `telegram`, `ntfy`, `email`, or a custom string |
| `--external-id` / `-e` | String | Yes | — | Channel-specific target: Telegram chat_id, ntfy topic, email address |
| `--display-name` / `-d` | String | Yes | — | Human-readable label (e.g. `@johndoe`, `john@example.com`) |
| `--credential-key` / `-c` | String | No | `""` | Vault key holding the bot token or password |
| `--reply-topic` | String | No | — | ntfy: topic to listen on for inbound replies |
| `--server-url` | String | No | — | ntfy: base server URL (default: `https://ntfy.sh`) |

**Telegram example:**

First store the bot token in the vault:
```bash
agentctl secret set TELEGRAM_BOT_TOKEN
```

Then connect the channel (chat_id is your numeric Telegram chat ID):
```bash
agentctl channel connect \
  --kind telegram \
  --external-id "123456789" \
  --display-name "@myhandle" \
  --credential-key TELEGRAM_BOT_TOKEN
```

**ntfy example:**

```bash
agentctl channel connect \
  --kind ntfy \
  --external-id "my-agentos-alerts" \
  --display-name "ntfy/my-agentos-alerts" \
  --reply-topic "my-agentos-replies" \
  --server-url "https://ntfy.sh"
```

**Email example:**

```bash
agentctl secret set EMAIL_PASSWORD
agentctl channel connect \
  --kind email \
  --external-id "ops-team@example.com" \
  --display-name "ops-team@example.com" \
  --credential-key EMAIL_PASSWORD
```

### `channel list`

List all registered channels.

*No flags.*

**Example:**
```bash
agentctl channel list
```

**Output columns:** `CHANNEL ID` (full UUID), `KIND`, `DISPLAY NAME`, `EXTERNAL ID`, `CONNECTED`

### `channel test`

Send a test notification to a registered channel to verify delivery.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | String | Channel UUID (from `channel list`) |

**Example:**
```bash
agentctl channel test a3b2c1d0-1234-5678-9abc-def012345678
```

### `channel disconnect`

Remove a registered channel.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | String | Channel UUID (from `channel list`) |

**Example:**
```bash
agentctl channel disconnect a3b2c1d0-1234-5678-9abc-def012345678
```

---

## End-to-End Workflow

A typical agent notification workflow:

```bash
# 1. Store Telegram bot token in vault
agentctl secret set TELEGRAM_BOT_TOKEN

# 2. Register the Telegram channel
agentctl channel connect \
  --kind telegram \
  --external-id "123456789" \
  --display-name "@ops-handle" \
  --credential-key TELEGRAM_BOT_TOKEN

# 3. Verify the channel is working
agentctl channel test <channel-id>

# 4. Connect an agent with notify permission
agentctl agent connect \
  --provider anthropic \
  --model claude-sonnet-4-6 \
  --name worker \
  --grant user.notify:w \
  --grant user.interact:x

# 5. Run a task — the agent sends notifications automatically
agentctl task run --agent worker "Audit the user database and ask me before deleting anything"

# 6. Watch for incoming questions in another terminal
agentctl notifications watch

# 7. When a question arrives, respond to it
agentctl notifications respond <notification-id> --response "Yes, proceed"
```

---

## Permissions Required

| Tool | Permission | Format |
|------|-----------|--------|
| `notify-user` | Write to user inbox | `user.notify:w` |
| `ask-user` | Interactive user questions | `user.interact:x` |

Grant at connect time:
```bash
agentctl agent connect --provider ollama --model llama3.2 --name worker \
  --grant user.notify:w --grant user.interact:x
```

Or after connection:
```bash
agentctl perm grant worker user.notify:w
agentctl perm grant worker user.interact:x
```

---

## Related

- [[04-CLI Reference Complete]] — Full CLI reference including `notifications` and `channel` commands
- [[07-Tool System]] — `ask-user` and `notify-user` tool details
- [[08-Security Model]] — Capability tokens and permission enforcement
- [[09-Secrets and Vault]] — Storing channel credentials securely
