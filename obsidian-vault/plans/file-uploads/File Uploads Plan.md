---
title: File Uploads — Ecosystem-Wide File Management
tags:
  - webui
  - agents
  - tools
  - v3
  - plan
date: 2026-04-17
status: in-progress
effort: 3d
priority: high
---

# File Uploads — Ecosystem-Wide File Management

> Users upload files once; agents read them any time; chat and tasks reference them by @mention or attachment.

---

## Why This Matters

Agents currently can only access files in `data_dir` or workspace paths hardcoded at startup. Users have no way to hand documents, PDFs, CSVs, or code snippets to the system without SSH access. This blocks real-world use cases like "analyze this report", "summarize these meeting notes", "parse this CSV". The feature closes this gap by giving every actor in AgentOS — users, chat, tasks, and agents — a shared, durable file namespace.

## Current State

| Component | State |
|-----------|-------|
| File tools (reader/writer/glob/grep) | Operate on `data_dir` and workspace paths only |
| Web UI | No upload UI anywhere |
| Chat | No attachment support |
| Agent tools | No access to user-uploaded files |
| Task prompts | No way to reference files by name |

## Target Architecture

```
Browser Upload ──────────────────────────────────────────────────┐
                                                                 ▼
                              FileStore (SQLite @ data_dir/uploads/file_registry.db)
                                     │ id, name, mime, size, path, tags
                                     │
  ┌──────────────────────────────────┼──────────────────────────────────┐
  │                                  │                                  │
  ▼                                  ▼                                  ▼
/files page                 Chat send handler              UserFileReaderTool
(list + upload)          (@mention + attachment)          (agents read any time)
                                                      data_dir/uploads/file_registry.db
```

## Phase Overview

| Phase | Name | Effort | Detail Doc | Status |
|-------|------|--------|------------|--------|
| 1 | FileStore & types | 0.5d | [[01-file-store]] | complete |
| 2 | Web handlers & routes | 0.5d | [[02-web-handlers]] | complete |
| 3 | UI templates | 0.5d | [[03-ui-templates]] | complete |
| 4 | Agent file reader tool | 0.5d | [[04-agent-tool]] | complete |
| 5 | Chat attachment & @mention | 1d | [[05-chat-integration]] | complete |

## Key Design Decisions

1. **FileStore lives in the web layer** — like `ChatStore`, it's a `Mutex<Connection>` SQLite store initialized by `WebServer`. The web layer owns uploads; agents reach them via a dedicated tool.
2. **Agent access via `UserFileReaderTool`** — tool opens `data_dir/uploads/file_registry.db` directly at execute time (stateless, no shared state needed). This avoids adding a cross-crate registry or kernel bus commands.
3. **Files stored as `{uuid}_{safe_name}` on disk** — UUID prefix makes paths unguessable; the original filename is preserved in the registry for display and @mention lookup.
4. **Chat attachment is two-step** — JS uploads the file via `POST /api/files/upload` (returns JSON `{id,name}`), then the send form carries `file_ids` as a comma-separated hidden input. Server-side resolution prepends file content to the LLM context.
5. **@mention resolution** — `@filename` in chat messages is resolved server-side: the filename is looked up in the registry, content is prepended to the message. Works for both chat sessions and task prompts.
6. **Max file size: 100 MiB** — enforced at the web layer via streaming chunk accumulation. Agent tool has its own 50 MiB read limit.
7. **Security** — UUID-validated IDs, canonicalized path containment check for downloads, CSRF validation for uploads, filename sanitization (strip `..`, `/`, `\`).

## Risks

| Risk | Mitigation |
|------|-----------|
| Disk exhaustion from large uploads | 100 MiB per-file cap; user can delete via /files |
| Path traversal via crafted filenames | Strip `..`, `/`, `\` from filenames; stored name is `{uuid}_{sanitized}` |
| Agents reading sensitive uploads | FileStore registry is in `data_dir` — requires `fs.user_data:r` permission |
| Binary files breaking LLM context | Tool returns base64 for non-text; chat handler skips binary content injection |
