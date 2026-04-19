---
title: Phase 9 — Approval Workflow UX
tags:
  - mobile
  - approvals
  - escalations
  - push
  - phase-9
date: 2026-04-19
status: planned
effort: 3d
priority: critical
---

# Phase 9 — Approval Workflow UX

> The flagship mobile UX: a push notification arrives, user taps "Approve" or "Deny" directly from the lock screen, and the blocked task resumes. In-app Approvals tab shows history and lets users dig into context. Biometric unlock for approvals is planned (v2) but the plumbing is added here.

---

## Why this phase

Interactive approval (OpenClaw Phase 6) is the biggest "pager"-style use case for AgentOS mobile. A high-risk tool call blocks until a human says yes/no; users don't sit at a laptop waiting. Mobile push + one-tap resolution turns a minutes-long block into seconds.

## Current → Target state

**Current:** Phase 3 delivers push plumbing and a `MobilePushHook` that fires on `EscalationCreated`. Phase 4 delivers `POST /v1/escalations/:id/resolve`.

**Target:**
- **Notification category `ESCALATION`** registered on iOS + Android with two actions: `Approve` (green) and `Deny` (red, destructive).
- **Background handlers** that call the resolve endpoint without opening the app, when the user taps an action on the lock screen.
- **In-app Approvals tab**:
  - Sections: `Pending`, `Resolved today`, `Auto-denied`.
  - Tap pending → detail sheet: tool name, agent, task link, input preview (JSON syntax-highlighted), risk class pill, auto-deny countdown.
  - `Approve` / `Deny` buttons in sheet; deny requires typing 2-char reason (optional).
  - Real-time updates via SSE subscription to `/v1/escalations/stream` (new endpoint).
- **Auto-deny countdown** visible everywhere an escalation appears; once ≤ 30s, row pulses red.
- **Missed approval** surface: if a push action fails or the escalation auto-denied, an in-app card at the top of Tasks/Approvals summarizes it and links to audit log.
- **Biometric gate (plumbed, off by default)**: Setting "Require Face ID / fingerprint to approve". When on, approve/deny goes through `expo-local-authentication` first.

## Detailed subtasks

### 9.1 Notification categories

File: `mobile/src/notifications/categories.ts`.

```ts
import * as Notifications from 'expo-notifications';

export async function registerCategories() {
  await Notifications.setNotificationCategoryAsync('ESCALATION', [
    { identifier: 'APPROVE', buttonTitle: 'Approve', options: { isDestructive: false, opensAppToForeground: false } },
    { identifier: 'DENY',    buttonTitle: 'Deny',    options: { isDestructive: true,  opensAppToForeground: false } },
  ]);
}
```

Call from `app/_layout.tsx` on mount. Server-side Phase 3 sends `data.category_id = 'ESCALATION'`.

### 9.2 Background response handler

File: `mobile/src/notifications/responder.ts`.

```ts
Notifications.addNotificationResponseReceivedListener(async (resp) => {
  const { actionIdentifier, notification } = resp;
  const data = notification.request.content.data as { kind?: string; id?: string };
  if (data.kind !== 'escalation' || !data.id) return;
  const decision = actionIdentifier === 'APPROVE' ? 'approve' : actionIdentifier === 'DENY' ? 'deny' : null;
  if (!decision) return;   // default tap (no action) — opens app to detail

  try {
    const r = await apiFetch(`/v1/escalations/${data.id}/resolve`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ decision }),
    });
    if (!r.ok) throw new Error(`status ${r.status}`);
    await Notifications.scheduleNotificationAsync({
      content: { title: `${decision === 'approve' ? '✓ Approved' : '✗ Denied'}`, body: notification.request.content.title ?? '' },
      trigger: null,
    });
  } catch (e) {
    await Notifications.scheduleNotificationAsync({
      content: { title: 'Approval failed', body: 'Tap to open AgentOS and retry.' , data },
      trigger: null,
    });
  }
});
```

Register early in `app/_layout.tsx`. Handles both foreground and background (iOS: `opensAppToForeground: false` keeps the phone locked; Android: action buttons fire the handler directly).

### 9.3 Biometric gate (plumbed)

File: `mobile/src/notifications/biometric.ts`.

```ts
import * as LocalAuthentication from 'expo-local-authentication';

export async function gateApproval(): Promise<boolean> {
  const prefs = await loadSettings();
  if (!prefs.requireBiometric) return true;
  const supported = await LocalAuthentication.hasHardwareAsync();
  if (!supported) return true;
  const r = await LocalAuthentication.authenticateAsync({
    promptMessage: 'Approve tool call',
    disableDeviceFallback: false,
  });
  return r.success;
}
```

**Note:** background lock-screen actions skip biometrics — iOS/Android don't reliably prompt from a notification action. The setting applies only to in-app approve/deny. Document this clearly in Settings.

### 9.4 Approvals tab

File: `mobile/app/(main)/approvals.tsx`.

```tsx
const pending = useEscalations({ status: 'pending' });
const resolved = useEscalations({ status: 'resolved', since: startOfToday() });
const autoDenied = useEscalations({ status: 'auto_denied', since: startOfToday() });

return (
  <SectionList
    sections={[
      { title: 'Pending', data: pending.data ?? [] },
      { title: 'Resolved today', data: resolved.data ?? [] },
      { title: 'Auto-denied today', data: autoDenied.data ?? [] },
    ]}
    renderItem={({ item }) => <EscalationRow escalation={item} onPress={() => openSheet(item)} />}
    refreshing={pending.isRefetching}
    onRefresh={() => { pending.refetch(); resolved.refetch(); autoDenied.refetch(); }}
  />
);
```

Subscribe to `/v1/escalations/stream` once on mount; push events into react-query cache via `queryClient.setQueryData`. On pending-list change, badge the tab icon.

### 9.5 Escalation detail sheet

Renders:
- Tool name (big), risk class pill (`EXEC_CAPABLE`, `WRITE_SCOPED`, etc.).
- Agent avatar + name, task title (tap → task detail).
- Input preview: JSON tree (collapsed, expand on tap).
- Countdown to auto-deny (mm:ss, red when ≤ 30s).
- `Approve` (green) / `Deny` (red destructive). Deny opens inline reason box (max 140 chars).

Call `gateApproval()` before posting. On success, invalidate queries; sheet closes with a haptic confirm.

### 9.6 Escalation stream endpoint

File: `crates/agentos-api/src/handlers/escalations.rs`.

```rust
#[utoipa::path(get, path = "/v1/escalations/stream")]
pub async fn stream(Extension(p): Extension<AuthPrincipal>, State(s): State<AppState>) -> impl IntoResponse {
    let user_id = p.user_id()?;
    let rx = s.kernel.subscribe_escalation_events(user_id).await?;
    Sse::new(BroadcastStream::new(rx).map(|e| Ok(Event::default().json_data(e?)?)))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
```

Backed by a kernel broadcast channel fed by the existing `EscalationCreated`/`EscalationResolved`/`EscalationAutoDenied` audit events. Only events whose escalation's owning user matches `user_id` reach the subscriber.

### 9.7 Missed-approval surfacing

If a notification action returns non-OK (e.g., 409 "already resolved"), enqueue a local notification:

> Approval **failed** — tap to open

Tapping it routes to the approvals detail sheet, where the user sees the final state (likely auto-denied). Record this in a `notifications_missed` event for analytics.

### 9.8 Settings integration

Settings screen gains:
- Notification preferences (toggles from Phase 3): escalations, task failures, completions.
- Quiet hours picker.
- Biometric toggle + "learn more" explainer.
- "Test push" button → calls `POST /v1/devices/:id/test-push` (add this endpoint).

## Files changed

| File | Change |
|------|--------|
| `mobile/app/_layout.tsx` | register categories + response listener on mount |
| `mobile/app/(main)/approvals.tsx` | tab screen |
| `mobile/src/notifications/{categories,responder,biometric}.ts` | new |
| `mobile/src/approvals/{EscalationRow,EscalationSheet,CountdownChip}.tsx` | components |
| `mobile/src/api/queries.ts` | `useEscalations`, `useEscalationStream` |
| `mobile/app/(main)/settings.tsx` | push prefs + biometric + test push |
| `crates/agentos-api/src/handlers/escalations.rs` | `stream` + `POST /v1/devices/:id/test-push` |
| `crates/agentos-kernel/src/kernel.rs` | broadcast channel for escalation events |
| `mobile/package.json` | add `expo-local-authentication` |

## Dependencies

- [[03-device-registration-and-push-relay]] — delivery pipe
- [[04-mobile-api-surface-audit]] — escalation endpoints
- [[05-mobile-app-scaffold-and-auth]] — scaffold
- Kernel's existing `PendingEscalation` + `ApprovalHook` (OpenClaw Phase 6)

## Test plan

- Unit: `responder.ts` — `APPROVE` action calls POST resolve with `decision=approve`; 409 enqueues missed-approval notif.
- Unit: `gateApproval` — with setting off, returns true; with setting on and hardware absent, returns true (don't lock out users).
- Unit: Countdown chip transitions pulse-red at ≤ 30s remaining.
- Integration: Local mock Expo server sends escalation push; Maestro taps `Approve` on simulated notification; assert POST to resolve endpoint.
- Integration: `/v1/escalations/stream` emits an event when a new escalation is created for that user only (not for other users').
- E2E (Maestro): Open app → Approvals tab → tap pending → approve → row moves to Resolved section.
- Security: another user's escalation is NOT visible in this user's stream.

## Verification

```bash
cd mobile
npx tsc --noEmit
npx jest src/notifications src/approvals
cargo test -p agentos-api handlers::escalations
cargo test -p agentos-kernel escalation_subscription
```

## Related

- [[Mobile App Plan]]
- [[Mobile App Data Flow]] — §5 Approval flow
- [[03-device-registration-and-push-relay]]
- [[10-distribution-and-release]]
