---
title: Phase 8 — Workflow / Pipeline Builder
tags:
  - mobile
  - pipelines
  - workflow
  - phase-8
date: 2026-04-19
status: planned
effort: 5d
priority: medium
---

# Phase 8 — Workflow / Pipeline Builder

> Linear pipeline editor on mobile: create a pipeline by stacking steps (agent tasks, tool calls, waits, conditionals), save, run, and monitor via SSE. Linear-only on mobile; DAGs remain web-only until we have a better small-screen canvas.

---

## Why this phase

Pipelines are AgentOS's multi-step workflow primitive. A mobile builder lets users stitch common automations (e.g., "every morning at 9am summarize inbox and message me the result") without opening a laptop. Keeping it linear keeps the UX tractable on a phone.

## Current → Target state

**Current:** `(main)/pipelines.tsx` is a placeholder. Backend JSON pipeline creation lands in Phase 4.

**Target:**
- **Pipeline list** — `GET /v1/pipelines`, rows show name, step count, last run status.
- **Pipeline editor screen**:
  - Reorderable list of steps (`react-native-draggable-flatlist`).
  - "+" adds a step; picker offers step types: `Agent task`, `Tool call`, `Wait`, `Condition (if/else)`.
  - Per-step editor (bottom sheet):
    - Agent task: agent picker + goal + thinking level
    - Tool call: tool picker + JSON args editor (schema-validated against tool manifest)
    - Wait: duration picker (5s / 1m / 5m / 1h / custom)
    - Condition: simple expression builder (`${step_2.output.success} == true`)
  - Step output binding: `${step_N.field}` pickers populate from schema of prior step.
- **Save** → `POST /v1/pipelines` (create) or `PUT /v1/pipelines/:id` (update).
- **Run** → `POST /v1/pipelines/:id/run`; navigate to run-detail screen (SSE progress).
- **Run detail** — step-by-step timeline, each step expands to show input, output, cost; step rows link to underlying task detail (Phase 7).
- **Templates** — built-in starter templates: "Morning briefing", "Inbox triage", "Weekly summary". Tap → pre-fills editor. Templates live in `skills/core/pipelines/*.toml` and are fetched from `GET /v1/pipelines/templates`.

Out of scope for mobile v1 (web only):
- Arbitrary DAG editing
- Parallel-fan-out steps
- Custom step plugins

## Detailed subtasks

### 8.1 Editor state model

File: `mobile/src/pipelines/editor.ts`.

```ts
type StepDraft =
  | { id: string; kind: 'agent_task'; agent_id?: string; goal: string; thinking: ThinkingLevel }
  | { id: string; kind: 'tool_call'; tool_name: string; args: unknown }
  | { id: string; kind: 'wait'; duration_seconds: number }
  | { id: string; kind: 'condition'; expr: string; then_steps: StepDraft[]; else_steps: StepDraft[] };

type EditorState = {
  pipelineId?: string;
  name: string;
  description: string;
  steps: StepDraft[];
  dirty: boolean;
};
```

zustand store with undo (stack of prior states, capped at 20). Auto-save to secure-store every 5s if `dirty` so accidental app kill doesn't lose work.

### 8.2 Drag-to-reorder list

`react-native-draggable-flatlist` with haptic feedback on drag-start/drop. Step rows show:

```
┌──────────────────────────────────┐
│ 1. 🧠 agent_task                  │
│    sales-analyst · "Summarize…"  │
└──────────────────────────────────┘
```

Swipe-left reveals Delete, swipe-right reveals Duplicate.

### 8.3 Step editor bottom sheet

One sheet component with a `Switch`-on-`kind` renderer. Agent task form:

```tsx
<AgentPicker value={step.agent_id} onChange={...} />
<TextInput multiline value={step.goal} onChange={...} placeholder="What should this agent do?" />
<ThinkingSegmented value={step.thinking} onChange={...} />
```

Tool call form uses `useToolManifest(toolName)` to drive a schema-aware JSON editor. For simple scalar args we render actual form fields; for complex args we fall back to a JSON `TextInput` with inline validation (zod-from-jsonschema).

Variable-binding picker: tap a field → opens dropdown listing `${step_N.OUTPUT_FIELD}` candidates based on prior steps' output schemas.

### 8.4 Save mutation

```ts
async function savePipeline(state: EditorState) {
  const body = {
    name: state.name, description: state.description,
    steps: state.steps.map(toWirePipelineStep),
  };
  const r = state.pipelineId
    ? await apiFetch(`/v1/pipelines/${state.pipelineId}`, { method: 'PUT', body: JSON.stringify(body) })
    : await apiFetch('/v1/pipelines', { method: 'POST', body: JSON.stringify(body) });
  if (!r.ok) throw await r.json();
  return r.json();
}
```

Validation before save: every step must be complete. Show inline errors on incomplete rows.

### 8.5 Run + monitor

"Run" button → `POST /v1/pipelines/:id/run` returns `{ run_id }`. Navigate to `pipelines/[id]/runs/[runId].tsx`:

```ts
useEffect(() => {
  const ctrl = new AbortController();
  streamPipelineRun(runId, ctrl, { onEvent: (e) => setEvents(p => [...p, e]) });
  return () => ctrl.abort();
}, [runId]);
```

Event shape: `{ step_index, step_id, kind: 'started'|'finished'|'failed', task_id?, cost?, error? }`.

Each step row mirrors its event-log state:

- Pending (grey)
- Running (blue, spinner)
- Finished (green, tap → task detail)
- Failed (red, tap → error sheet with retry)

### 8.6 Templates

File: `crates/agentos-skills/src/pipeline_templates.rs` (new). Reads `skills/core/pipelines/*.toml` like:

```toml
id = "morning-briefing"
name = "Morning briefing"
description = "Daily digest delivered at 9am."
steps = [
  { kind = "agent_task", agent = "inbox-summarizer", goal = "Summarize overnight email" },
  { kind = "agent_task", agent = "calendar-reviewer", goal = "Surface meeting conflicts" },
  { kind = "tool_call", tool = "channel_send", args = { channel = "mobile_push", body = "${step_1.output.summary}\n\n${step_2.output.conflicts}" } },
]
```

Endpoint `GET /v1/pipelines/templates` returns this catalog. "Use template" clones it into the editor with a fresh id.

### 8.7 Scheduled runs (stretch)

Existing event-trigger system (`obsidian-vault/plans/agentos-event-trigger-system.md`) already supports cron-like triggers. Add a compact UI: tap "Schedule" on a saved pipeline → pick cron spec → `POST /v1/triggers` wires it. (Defer to a follow-up if time-constrained; not blocking for v1.)

## Files changed

| File | Change |
|------|--------|
| `mobile/app/(main)/pipelines.tsx` | list screen |
| `mobile/app/(main)/pipelines/new.tsx` + `[id].tsx` | editor |
| `mobile/app/(main)/pipelines/[id]/runs/[runId].tsx` | run detail |
| `mobile/src/pipelines/{editor,stream}.ts` | state + SSE helper |
| `mobile/src/pipelines/components/{StepRow,StepSheet,AgentPicker,ToolArgsEditor,TemplatePicker}.tsx` | components |
| `mobile/src/api/queries.ts` | pipeline hooks |
| `crates/agentos-api/src/handlers/pipelines.rs` | JSON create/update, run, stream (some from Phase 4) |
| `crates/agentos-skills/src/pipeline_templates.rs` | new |
| `skills/core/pipelines/*.toml` | 3 starter templates |
| `mobile/package.json` | add `react-native-draggable-flatlist`, `json-schema-to-zod` |

## Dependencies

- [[04-mobile-api-surface-audit]] — JSON pipeline endpoints + SSE
- [[05-mobile-app-scaffold-and-auth]]
- [[07-task-management-screens]] — run-detail links into task detail

## Test plan

- Unit: Editor zustand store supports undo/redo; dirty flag flips on mutation.
- Unit: `toWirePipelineStep` produces server-compatible JSON for all 4 kinds.
- Unit: Template clone generates fresh step ids.
- Integration: Create → save → run → SSE → all steps finished; cost sum matches sum of step costs.
- Integration: Condition step with false expr skips `then_steps`, executes `else_steps`.
- E2E (Maestro): pick "Morning briefing" template → rename → run → wait → see all 3 steps green.

## Verification

```bash
cd mobile
npx tsc --noEmit
npx jest src/pipelines
cargo test -p agentos-skills pipeline_templates
cargo test -p agentos-api handlers::pipelines
```

## Related

- [[Mobile App Plan]]
- [[Mobile App Data Flow]] — §4 Pipeline flow
- [[07-task-management-screens]]
- [[agentos-event-trigger-system]]
