---
title: File Attachment Known Issues
tags:
  - web
  - chat
  - file-upload
  - blocker
  - deferred
date: 2026-04-19
status: partial
effort: unknown
priority: medium
---

# File Attachment Known Issues

Two critical issues discovered during code review of the file attachment feature for chat. Both require schema/infrastructure changes and are deferred pending larger architectural work.

## C1: Multi-turn History Loses File Context

**Problem:** When a file is attached to a message, it's expanded into the LLM prompt for that turn only. On subsequent turns, prior file-attached messages are reconstructed from the DB but contain only the original text — the file content is lost.

**Scenario:**

1. **Turn 1:** User attaches `report.csv`, sends "summarize this"
   - DB stores: `message = "summarize this"` (original only)
   - LLM context: `<user_data filename="report.csv">{csv content}</user_data>\n---\nsummarize this`
   - Assistant replies with good summary ✓

2. **Turn 2:** User sends "now translate that to German" (no attachment)
   - Handler loads history from DB: `[("user", "summarize this"), ("assistant", "summary..."), ("user", "now translate...")]`
   - **CSV is gone.** LLM only sees "summarize this" with no file context
   - Model cannot process request properly ✗

**Root cause:** 
- `chat_messages` table stores only `content TEXT`, not `content_for_llm`
- History assembly in `send` handler (line 509-526 in `chat.rs`) reads the original `message` from DB
- File expansion happens only in the current turn, never persisted
- On turn N+1, reconstructing history loses turn N's file context

**Fix:**

Add one of:

- **Option A:** `content_for_llm TEXT` column on `chat_messages`
  - Store both display version (original) and LLM version (expanded)
  - History assembly reads `content_for_llm`
  - Cost: ~+200 bytes per message (duplication)

- **Option B:** `file_ids TEXT` column on `chat_messages`
  - Store the CSV of attached file UUIDs per message
  - History assembly calls `resolve_file_ids_to_context` for each prior turn
  - Cost: Re-expansion on every turn (re-reads files from disk)
  - Benefit: Stays in sync if files are modified/deleted

**Impact:**

- **Single-turn use cases:** No impact. "Analyse this file" works fine.
- **Multi-turn with re-attachment:** Works. User can re-attach the file.
- **Multi-turn without re-attachment:** Broken. Model loses context.

**Blocks:** Multi-turn file analysis workflows, persistent file reference semantics.

---

## C2: No File Ownership / Multi-user Scoping

**Problem:** The `uploaded_files` SQLite table has no `owner` column. Any authenticated user can enumerate/guess file UUIDs and inline another user's uploads into their own chat.

**Current schema:**

```sql
CREATE TABLE IF NOT EXISTS uploaded_files (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    original_name TEXT NOT NULL,
    mime          TEXT NOT NULL DEFAULT 'application/octet-stream',
    size          INTEGER NOT NULL DEFAULT 0,
    path          TEXT NOT NULL,
    tags          TEXT NOT NULL DEFAULT '',
    uploaded_at   TEXT NOT NULL
    -- MISSING: owner, user_id, session_id
);
```

**Vulnerability:**

1. User A uploads `secret_numbers.csv` (UUID: `550e8400-e29b-41d4-a716-446655440000`)
2. User B guesses/obtains the UUID somehow (from a link, audit logs, etc.)
3. User B crafts a chat message with `file_ids=550e8400-e29b-41d4-a716-446655440000`
4. Server calls `resolve_file_ids_to_context()` without ownership check
5. `FileStore::get_files_by_ids()` queries `WHERE id IN (?)` with no scope
6. User B gets User A's file content inlined into their LLM prompt
7. Response containing User A's secrets gets saved in User B's transcript

**Root cause:**

- AgentOS has no user model (no login, no `users` table)
- Every session is treated as the same implicit owner
- `resolve_file_ids_to_context` blindly resolves any UUID

**Fix:**

Requires a user/session identity system:

1. Add `owner_session_id TEXT NOT NULL` to `uploaded_files`
2. On upload, populate from the `agentos_session` cookie (via `csrf::session_key()`)
3. Update all lookups:
   - `get_files_by_ids()` → add `AND owner_session_id = ?` clause
   - `find_by_name()` → add `AND owner_session_id = ?` clause
4. Update `resolve_file_ids_to_context()` and `resolve_at_mentions()` to pass caller's session ID

**Cost:**

- Schema migration (ADD COLUMN on potentially large table)
- Session tracking infra (already exists via cookies)
- Plumbing session_id through the call stack

**Why deferred:**

AgentOS is currently single-user (no login system). Adding file ownership requires:
1. A proper user model (not just sessions)
2. Multi-tenant isolation throughout the app
3. Test fixtures for multi-user scenarios

This is blocked on broader auth/user work.

**Impact in single-user deployments:**

None. The system is fine when there's only one authenticated user.

**Impact in multi-user deployments:**

Critical. File uploads can leak between users. Any self-hosted instance shared among multiple people is vulnerable.

**Blocks:** Multi-user deployments, compliance (GDPR, HIPAA, SOC2 require data isolation).

---

## Timeline & Dependencies

Both issues have clean remediation paths but require infrastructure:

- **C1** blocks multi-turn file workflows; fix is a 1-2 day schema + query update
- **C2** blocks multi-user safety; fix depends on user model (larger effort)

**Recommendation:**

- **For now:** Single-user deployments are safe. Multi-user deployments should not use file attachments.
- **Before public release:** Implement C1 fix (option B is simpler) + user model + C2 fix.
- **MVP path:** Add a feature flag ` file_uploads_enabled` that defaults to false in multi-user setups.

---

## Related

- [[File Attachment Code Review]] (discovered during review 2026-04-19)
- [[Multi-Agent Coordination]] (touches session/user model)
- [[Strategic Roadmap]] (user model is a future phase)
