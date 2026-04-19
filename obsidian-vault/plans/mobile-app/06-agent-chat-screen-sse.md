---
title: Phase 6 — Agent Chat Screen (SSE)
tags:
  - mobile
  - chat
  - sse
  - phase-6
date: 2026-04-19
status: planned
effort: 3d
priority: high
---

# Phase 6 — Agent Chat Screen (SSE)

> Build the main chat experience: list agents, pick one, start or resume a conversation, stream responses token-by-token via SSE, with safe markdown rendering, tool-call indicators, and resilient reconnects.

---

## Why this phase

Chatting with agents is the single most-used feature in any mobile AI client. It exercises auth, streaming, persistence, and error handling end-to-end. Getting it solid surfaces most issues the other screens will face.

## Current → Target state

**Current:** `(main)/chat.tsx` is a placeholder.

**Target:**
- **Agents tab entry** — list user's registered agents from `GET /v1/agents`, tap to open a chat.
- **Conversation list** — `GET /v1/chat/conversations` scoped to selected agent; pull-to-refresh; `Start new`.
- **Conversation screen**:
  - Render message history from `GET /v1/chat/conversations/:id/messages`.
  - Composer at bottom; send via `POST /v1/chat/completions` with `stream=true`.
  - Token-by-token rendering using `@microsoft/fetch-event-source`.
  - Inline tool-call cards (`assistant` messages with `tool_calls`) showing tool name + status.
  - Reconnect-on-drop with `Last-Event-ID`.
  - Cancel in-flight stream (aborts fetch, server marks message partial).
- **Markdown rendering** via `react-native-markdown-display` with code-block syntax highlighting through `react-native-syntax-highlighter` — images disabled to avoid surprise exfil.
- **Network/auth errors** surface as toasts; session expiry returns user to login.

## Detailed subtasks

### 6.1 API queries

File: `mobile/src/api/queries.ts`.

```ts
import { useQuery, useInfiniteQuery } from '@tanstack/react-query';
import { apiFetch } from './client';

export function useAgents() {
  return useQuery({
    queryKey: ['agents'],
    queryFn: async () => (await apiFetch('/v1/agents')).json(),
  });
}

export function useConversations(agentId: string) {
  return useInfiniteQuery({
    queryKey: ['conversations', agentId],
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam }) => {
      const q = new URLSearchParams({ agent_id: agentId, limit: '50' });
      if (pageParam) q.set('cursor', pageParam);
      return (await apiFetch(`/v1/chat/conversations?${q}`)).json();
    },
    getNextPageParam: (last) => last.next_cursor,
  });
}
```

### 6.2 Streaming send helper

File: `mobile/src/chat/stream.ts`.

```ts
import { fetchEventSource } from '@microsoft/fetch-event-source';
import { useAuth } from '@/auth/store';
import { BASE_URL } from '@/config';

export type StreamHandlers = {
  onDelta: (text: string) => void;
  onToolCall: (tc: ToolCallDelta) => void;
  onDone: () => void;
  onError: (err: Error) => void;
};

export async function streamChat(
  body: ChatCompletionRequest,
  abort: AbortController,
  h: StreamHandlers,
) {
  let lastEventId: string | undefined;
  await fetchEventSource(`${BASE_URL}/v1/chat/completions`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${useAuth.getState().tokens!.access}`,
      'content-type': 'application/json',
      ...(lastEventId ? { 'last-event-id': lastEventId } : {}),
    },
    body: JSON.stringify({ ...body, stream: true }),
    signal: abort.signal,
    openWhenHidden: false,          // allow iOS background pause
    async onopen(r) {
      if (r.status === 401) throw new Error('unauth');
      if (!r.ok) throw new Error(`http ${r.status}`);
    },
    onmessage(ev) {
      lastEventId = ev.id || lastEventId;
      if (ev.data === '[DONE]') { h.onDone(); return; }
      const j = JSON.parse(ev.data);
      const delta = j.choices?.[0]?.delta;
      if (delta?.content) h.onDelta(delta.content);
      if (delta?.tool_calls?.length) for (const tc of delta.tool_calls) h.onToolCall(tc);
    },
    onerror(err) { h.onError(err); throw err; },  // throw → stop reconnect loop
  });
}
```

Auth handling: wrap `streamChat` so a 401 triggers the refresh flow from `apiFetch`, then retry once. (Factor refresh logic out of `client.ts` into a reusable helper.)

### 6.3 Conversation screen

File: `mobile/app/(main)/chat/[agentId]/[conversationId].tsx`.

Use a FlatList inverted, messages rendered bottom-up. State held by `useChatMessages(conversationId)` backed by react-query for the history and a local `useState` for the in-flight stream buffer. When the stream completes, invalidate and merge.

Composer: `TextInput` + send button. On send:

1. Optimistically append user message to list.
2. Create abort controller; store in ref.
3. Call `streamChat({ conversation_id, messages: [...last N, newMsg] })`.
4. On `onDelta`, append to a running `assistantBuffer`; force list to re-render throttled every 16ms (rAF).
5. On `onDone`, commit the assistant message, clear buffer, invalidate history.

Cancel button swaps in while streaming. Aborting calls `controller.abort()`; server records partial message.

### 6.4 Tool-call card

Mid-stream, assistant deltas may include `tool_calls`. Render a collapsible card inline:

```
┌─ tool: web_search                        ⏳
│   { "query": "agentos push notifications" }
└─ (awaiting result)
```

On `tool.result` delta, update card to `✓` and show a 2-line preview. Full result opens a sheet.

### 6.5 Markdown + code rendering

Prefer `react-native-markdown-display`. Whitelist elements (`p`, `h1-h3`, `ul`, `ol`, `code_inline`, `fence`, `link`). Links via `Linking.openURL` — confirm external URL first ("Open agentos.example.com in browser?").

Code fences via `react-native-syntax-highlighter` with `hljs`'s `atomOneDark` theme. Long lines break; copy-code button per fence (`Clipboard.setStringAsync`).

### 6.6 Reconnect + offline

- `fetchEventSource` reconnects automatically; we pass `Last-Event-ID` so server resumes from the right token.
- NetInfo listener: when `isConnected === false`, show a persistent banner. Aborts the current stream; does not auto-reconnect — user must tap "Retry".
- Conversations list is backed by react-query cache → works offline read-only.

### 6.7 Keyboard + UX

- `KeyboardAvoidingView` wraps the composer.
- Haptics on send (`expo-haptics`).
- Auto-scroll to bottom on new tokens when user hasn't scrolled up; otherwise show "new messages ↓" chip.
- Message long-press → copy text, regenerate (future), delete.

## Files changed

| File | Change |
|------|--------|
| `mobile/app/(main)/chat.tsx` | agent list screen |
| `mobile/app/(main)/chat/[agentId]/index.tsx` | conversation list for agent |
| `mobile/app/(main)/chat/[agentId]/new.tsx` | start new conversation |
| `mobile/app/(main)/chat/[agentId]/[conversationId].tsx` | conversation screen |
| `mobile/src/chat/stream.ts` | SSE helper |
| `mobile/src/chat/components/MessageBubble.tsx` | new |
| `mobile/src/chat/components/ToolCallCard.tsx` | new |
| `mobile/src/chat/components/Composer.tsx` | new |
| `mobile/src/api/queries.ts` | conversation/agent queries |
| `mobile/package.json` | add `@microsoft/fetch-event-source`, `react-native-markdown-display`, `react-native-syntax-highlighter`, `expo-haptics`, `@react-native-community/netinfo`, `expo-clipboard` |

## Dependencies

- [[05-mobile-app-scaffold-and-auth]] — scaffold + auth
- [[04-mobile-api-surface-audit]] — `/v1/chat/conversations` + SSE contract

## Test plan

- Unit: `streamChat` delivers `onDelta` for each chunk, `onDone` on `[DONE]`, `onError` on HTTP error.
- Unit: abort during stream clears buffer without leaking controller.
- Unit: markdown renderer strips `<script>` and inline JS.
- Integration (mocked server): 10-chunk response renders incrementally; final text matches concatenated chunks.
- Integration: 401 mid-stream triggers refresh + retry once, not loop.
- E2E (Maestro): send a message to a mock agent, assert streamed text appears.
- Accessibility: VoiceOver reads new messages; composer has `accessibilityLabel`.

## Verification

```bash
cd mobile
npx tsc --noEmit
npx jest src/chat
npx expo start    # manual smoke
```

## Related

- [[Mobile App Plan]]
- [[Mobile App Data Flow]] — §2 Chat SSE
- [[07-task-management-screens]] — tasks often start from chat
