---
title: Phase 7 — Task Management Screens
tags:
  - mobile
  - tasks
  - checkpoints
  - phase-7
date: 2026-04-19
status: planned
effort: 4d
priority: high
---

# Phase 7 — Task Management Screens

> Build task lifecycle UI: create a task, watch it run (SSE progress), inspect its tool calls and audit trail, and resume from a checkpoint. Surfaces the durable-task story that differentiates AgentOS from stateless chat apps.

---

## Why this phase

Tasks are the unit of durable work. Without a mobile UI for listing, creating, monitoring, and resuming them, the app is just a chat client. Checkpoint resume is a unique AgentOS feature worth a first-class screen.

## Current → Target state

**Current:** `(main)/tasks.tsx` is a placeholder.

**Target:**
- **Task list** — `GET /v1/tasks?status=...&cursor=...`, grouped by status tabs: `Running`, `Paused`, `Completed`, `Failed`. Pull-to-refresh, infinite scroll. Filter chip by agent.
- **Task detail screen**:
  - Header: title, agent, status pill, cost, elapsed.
  - Tabs: **Progress**, **Tool calls**, **Audit**, **Checkpoints**.
  - Progress: SSE live stream (`/v1/tasks/:id/stream`) rendering event timeline.
  - Tool calls: list of `ToolCallRecord` entries with I/O preview.
  - Audit: filtered audit-log slice for this task.
  - Checkpoints: list; tap to resume → `POST /v1/tasks/:id/resume`.
- **Create task sheet**: pick agent → enter goal → select `thinking` level (Off/Low/Medium/High/Max) → optional `skip_checkpoint` toggle → submit.
- **Running state polish**: progress bar, "cancel" button (`DELETE /v1/tasks/:id`), heartbeat indicator.

## Detailed subtasks

### 7.1 Task queries

File: `mobile/src/api/queries.ts` (extend).

```ts
export function useTasks(status?: TaskStatus) {
  return useInfiniteQuery({
    queryKey: ['tasks', status],
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam }) => {
      const q = new URLSearchParams({ limit: '30' });
      if (status) q.set('status', status);
      if (pageParam) q.set('cursor', pageParam);
      return (await apiFetch(`/v1/tasks?${q}`)).json();
    },
    getNextPageParam: (last) => last.next_cursor,
    refetchInterval: status === 'running' ? 5000 : false,
  });
}

export function useTask(id: string) {
  return useQuery({ queryKey: ['task', id], queryFn: async () => (await apiFetch(`/v1/tasks/${id}`)).json() });
}

export function useTaskCheckpoints(id: string) {
  return useQuery({ queryKey: ['task-checkpoints', id], queryFn: async () =>
    (await apiFetch(`/v1/tasks/${id}/checkpoints`)).json() });
}
```

### 7.2 Task list screen

File: `mobile/app/(main)/tasks.tsx`.

Use `@gorhom/segmented-control` (or Tailwind segmented chips) for the four status tabs. Each tab renders a FlatList of `TaskRow` cards:

```
┌─────────────────────────────────────────┐
│  Build weekly report                 ●  │  ← green dot = running
│  sales-analyst · 2m ago · $0.012        │
└─────────────────────────────────────────┘
```

Empty states per tab ("No running tasks", "No failures — nice!").

### 7.3 Task detail screen with live SSE

File: `mobile/app/(main)/tasks/[id].tsx`.

```tsx
function useTaskStream(taskId: string) {
  const [events, setEvents] = useState<TaskEvent[]>([]);
  useEffect(() => {
    const ctrl = new AbortController();
    streamTaskEvents(taskId, ctrl, {
      onEvent: (e) => setEvents((prev) => [...prev, e]),
      onError: (e) => { /* toast */ },
    });
    return () => ctrl.abort();
  }, [taskId]);
  return events;
}
```

`streamTaskEvents` mirrors `streamChat` but hits `/v1/tasks/:id/stream`. Events render as a timeline:

```
10:31:02  ▶ Started
10:31:05  🔧 tool:web_search  "agentos push"
10:31:07  ✅ tool:web_search  12 results
10:31:14  💾 checkpoint #3
10:31:22  ✓ Finished
```

### 7.4 Tool-calls tab

Query `GET /v1/tasks/:id/tool-calls` (adds a list endpoint backed by existing `ToolCallRecord` storage). Each row expands to show formatted input/output JSON (collapsed preview → full sheet on tap). Copy buttons for each JSON blob.

### 7.5 Audit tab

Query `GET /v1/audit?task_id=:id&cursor=...`. Render as a minimal log viewer. Filter toggle: "Show only security events" (trust-tier changes, capability checks, escalations). Link out to the related screen (e.g., escalation event → Approvals tab).

### 7.6 Checkpoints tab + resume

```tsx
function ResumeButton({ taskId, checkpointId }: Props) {
  const [busy, setBusy] = useState(false);
  async function resume() {
    setBusy(true);
    const r = await apiFetch(`/v1/tasks/${taskId}/resume`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ checkpoint_id: checkpointId }),
    });
    setBusy(false);
    if (!r.ok) { toast('Resume failed'); return; }
    router.replace(`/tasks/${taskId}`);
  }
  return <Button onPress={resume} busy={busy}>Resume from this checkpoint</Button>;
}
```

Confirmation sheet: "Resume from checkpoint #3 (10:31:14)? Any work after this point will be discarded."

### 7.7 Create task sheet

File: `mobile/src/tasks/CreateTaskSheet.tsx`.

Bottom sheet form (`@gorhom/bottom-sheet`). Fields:
- Agent: `react-native-select-dropdown` populated from `useAgents()`.
- Goal: multi-line `TextInput`, zod schema requires ≥ 4 chars.
- Thinking: segmented `Off / Low / Medium / High / Max`.
- Advanced: toggle `skip_checkpoint` (off by default).

Submit:

```ts
const res = await apiFetch('/v1/tasks', {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ agent_id, goal, thinking, skip_checkpoint }),
});
```

On success, navigate to detail screen.

### 7.8 Cancel

`DELETE /v1/tasks/:id`. Show destructive action sheet, confirm, then call. Success → invalidate list query.

### 7.9 Cost + quota display

Header shows cumulative cost (`task.cost_usd`), formatted to 4 decimal places. If `task.cost_usd > agent.budget * 0.8`, show orange warning; over-budget shows red. (Relies on existing `cost_tracker.rs`.)

## Files changed

| File | Change |
|------|--------|
| `mobile/app/(main)/tasks.tsx` | list + create FAB |
| `mobile/app/(main)/tasks/[id].tsx` | detail screen w/ tab nav |
| `mobile/src/tasks/{TaskRow,TaskHeader,EventTimeline,ToolCallsList,AuditList,CheckpointsList,CreateTaskSheet}.tsx` | components |
| `mobile/src/tasks/stream.ts` | task SSE helper |
| `mobile/src/api/queries.ts` | task-related hooks |
| `crates/agentos-api/src/handlers/tasks.rs` | add `GET /v1/tasks/:id/tool-calls` |
| `mobile/package.json` | add `@gorhom/bottom-sheet`, `react-native-select-dropdown` |

## Dependencies

- [[04-mobile-api-surface-audit]] — `/v1/tasks/:id/stream`, pagination, OpenAPI
- [[05-mobile-app-scaffold-and-auth]] — scaffold + auth
- [[06-agent-chat-screen-sse]] — reuse SSE infra pattern

## Test plan

- Unit: `useTasks` with `status=running` refetches every 5s.
- Unit: Resume confirmation sheet dispatches `POST /v1/tasks/:id/resume` with correct body.
- Integration: mock SSE feeds `Started → Tool → Finished`; timeline renders 3 rows.
- Integration: cancel sends DELETE; list invalidates.
- E2E (Maestro): create task → see it in Running → wait for completion → inspect Audit tab.
- Accessibility: status pills have `accessibilityLabel` (e.g., "Status: running").

## Verification

```bash
cd mobile
npx tsc --noEmit
npx jest src/tasks
cargo test -p agentos-api handlers::tasks
```

## Related

- [[Mobile App Plan]]
- [[Mobile App Data Flow]] — §3 Task flow
- [[09-approval-workflow-ux]] — escalations interrupt running tasks
