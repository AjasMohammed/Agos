---
title: Phase 5 — Mobile App Scaffold & Auth Flow
tags:
  - mobile
  - react-native
  - expo
  - auth
  - phase-5
date: 2026-04-19
status: planned
effort: 4d
priority: high
---

# Phase 5 — Mobile App Scaffold & Auth Flow

> Create the React Native (Expo) project under `mobile/`, wire the generated OpenAPI client, implement OAuth2 + PKCE login using `expo-auth-session`, secure-store token persistence, and an app shell with tab navigation. Produces a minimal app that can log in, display the logged-in user, and log out.

---

## Why this phase

Every remaining mobile phase plugs screens into this scaffold. Getting the foundations right — typed API client, token refresh interceptor, navigation shell, secure storage — removes rework later.

## Current → Target state

**Current:** No mobile directory in the repo.

**Target:**
- `mobile/` directory with an Expo SDK 52 TypeScript project.
- `mobile/src/api/` — generated OpenAPI client + hand-written HTTP wrapper (`fetch` + auth header + refresh-on-401).
- `mobile/src/auth/` — PKCE flow using `expo-auth-session`, secure-store persistence (`expo-secure-store`).
- `mobile/app/` (Expo Router) — tab navigation shell with five tabs: Chat, Tasks, Pipelines, Approvals, Settings. All but Settings render a "coming soon" placeholder in this phase.
- `mobile/src/config.ts` — reads `EXPO_PUBLIC_AGENTOS_BASE_URL` (default `https://agentos.local`).
- NativeWind configured for styling.
- A working end-to-end: open app → tap "Log in" → in-app browser → log in → back to app → see user email → log out.

## Detailed subtasks

### 5.1 Bootstrap Expo project

From `/home/ajas/Desktop/agos`:

```bash
npx create-expo-app@latest mobile --template blank-typescript
cd mobile
npx expo install \
  expo-router expo-linking expo-auth-session expo-crypto expo-secure-store expo-notifications \
  @microsoft/fetch-event-source @tanstack/react-query zustand \
  nativewind tailwindcss react-hook-form zod @hookform/resolvers
npm i -D openapi-typescript @types/node prettier eslint eslint-config-expo
```

Add `mobile/.prettierrc`, `mobile/.eslintrc.js`, and `mobile/tailwind.config.js` (NativeWind preset).

### 5.2 App structure

```
mobile/
├── app/                          # expo-router file-based routes
│   ├── _layout.tsx              # root Stack
│   ├── (auth)/
│   │   └── login.tsx
│   └── (main)/
│       ├── _layout.tsx          # Tabs
│       ├── chat.tsx             # placeholder
│       ├── tasks.tsx            # placeholder
│       ├── pipelines.tsx        # placeholder
│       ├── approvals.tsx        # placeholder
│       └── settings.tsx
├── src/
│   ├── api/
│   │   ├── generated.ts         # from Phase 4
│   │   ├── client.ts            # fetch wrapper w/ auth + refresh
│   │   └── queries.ts           # react-query hooks
│   ├── auth/
│   │   ├── store.ts             # zustand + secure-store
│   │   ├── pkce.ts              # PKCE helpers
│   │   └── hooks.ts             # useAuth, useLogin, useLogout
│   ├── config.ts
│   └── components/              # shared UI
├── openapi.json                 # copy of /v1/openapi.json
└── package.json
```

### 5.3 PKCE helpers

File: `mobile/src/auth/pkce.ts`.

```ts
import * as Crypto from 'expo-crypto';

function base64UrlEncode(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export async function generatePkce(): Promise<{ verifier: string; challenge: string }> {
  const randomBytes = await Crypto.getRandomBytesAsync(32);
  const verifier = base64UrlEncode(randomBytes);
  const digest = await Crypto.digestStringAsync(
    Crypto.CryptoDigestAlgorithm.SHA256,
    verifier,
    { encoding: Crypto.CryptoEncoding.BASE64 },
  );
  const challenge = digest.replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  return { verifier, challenge };
}
```

### 5.4 Auth store with secure-store persistence

File: `mobile/src/auth/store.ts`.

```ts
import { create } from 'zustand';
import * as SecureStore from 'expo-secure-store';

type Tokens = { access: string; refresh: string; expiresAt: number };

type AuthState = {
  user?: { id: string; email: string; displayName: string };
  tokens?: Tokens;
  hydrated: boolean;
  setSession: (s: { user: AuthState['user']; tokens: Tokens }) => Promise<void>;
  clearSession: () => Promise<void>;
  hydrate: () => Promise<void>;
};

const KEY_TOKENS = 'agentos.tokens.v1';
const KEY_USER = 'agentos.user.v1';

export const useAuth = create<AuthState>((set) => ({
  hydrated: false,
  async setSession({ user, tokens }) {
    await SecureStore.setItemAsync(KEY_TOKENS, JSON.stringify(tokens));
    await SecureStore.setItemAsync(KEY_USER, JSON.stringify(user));
    set({ user, tokens });
  },
  async clearSession() {
    await SecureStore.deleteItemAsync(KEY_TOKENS);
    await SecureStore.deleteItemAsync(KEY_USER);
    set({ user: undefined, tokens: undefined });
  },
  async hydrate() {
    const [t, u] = await Promise.all([
      SecureStore.getItemAsync(KEY_TOKENS),
      SecureStore.getItemAsync(KEY_USER),
    ]);
    set({
      tokens: t ? JSON.parse(t) : undefined,
      user: u ? JSON.parse(u) : undefined,
      hydrated: true,
    });
  },
}));
```

### 5.5 Login flow

File: `mobile/app/(auth)/login.tsx`.

```tsx
import { useRouter } from 'expo-router';
import * as WebBrowser from 'expo-web-browser';
import * as Linking from 'expo-linking';
import { generatePkce } from '@/auth/pkce';
import { useAuth } from '@/auth/store';
import { BASE_URL } from '@/config';

export default function Login() {
  const setSession = useAuth((s) => s.setSession);
  const router = useRouter();

  async function login() {
    const { verifier, challenge } = await generatePkce();
    const redirectUri = Linking.createURL('/auth-callback');
    const authUrl =
      `${BASE_URL}/v1/auth/authorize?response_type=code` +
      `&client_id=mobile&code_challenge=${challenge}` +
      `&code_challenge_method=S256&redirect_uri=${encodeURIComponent(redirectUri)}`;

    const result = await WebBrowser.openAuthSessionAsync(authUrl, redirectUri);
    if (result.type !== 'success') return;
    const code = new URL(result.url).searchParams.get('code');
    if (!code) return;

    const tokResp = await fetch(`${BASE_URL}/v1/auth/token`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        grant_type: 'authorization_code',
        code, code_verifier: verifier, redirect_uri: redirectUri,
      }),
    }).then((r) => r.json());

    const me = await fetch(`${BASE_URL}/v1/auth/me`, {
      headers: { authorization: `Bearer ${tokResp.access_token}` },
    }).then((r) => r.json());

    await setSession({
      user: { id: me.id, email: me.email, displayName: me.display_name },
      tokens: {
        access: tokResp.access_token,
        refresh: tokResp.refresh_token,
        expiresAt: Date.now() + tokResp.expires_in * 1000,
      },
    });
    router.replace('/');
  }
  return <Button onPress={login}>Log in</Button>;
}
```

### 5.6 HTTP client with auth + refresh

File: `mobile/src/api/client.ts`.

```ts
import { useAuth } from '@/auth/store';
import { BASE_URL } from '@/config';

let refreshInflight: Promise<void> | null = null;

async function refreshTokens() {
  const { tokens, setSession, clearSession, user } = useAuth.getState();
  if (!tokens) throw new Error('no refresh token');
  const r = await fetch(`${BASE_URL}/v1/auth/refresh`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ refresh_token: tokens.refresh }),
  });
  if (r.status === 401) { await clearSession(); throw new Error('session expired'); }
  const j = await r.json();
  await setSession({
    user,
    tokens: { access: j.access_token, refresh: j.refresh_token, expiresAt: Date.now() + j.expires_in * 1000 },
  });
}

export async function apiFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const { tokens } = useAuth.getState();
  const headers = new Headers(init.headers);
  if (tokens?.access) headers.set('authorization', `Bearer ${tokens.access}`);
  let resp = await fetch(`${BASE_URL}${path}`, { ...init, headers });
  if (resp.status === 401) {
    refreshInflight ??= refreshTokens().finally(() => (refreshInflight = null));
    try { await refreshInflight; } catch { return resp; }
    headers.set('authorization', `Bearer ${useAuth.getState().tokens!.access}`);
    resp = await fetch(`${BASE_URL}${path}`, { ...init, headers });
  }
  return resp;
}
```

### 5.7 App shell

Root layout (`app/_layout.tsx`) hydrates auth on mount, splits into `(auth)` / `(main)` stacks. Main tab layout uses `expo-router` `Tabs` with five placeholder tabs plus a real Settings tab (shows user, server URL, log-out button, device registration state).

### 5.8 Trust-on-first-use URL entry

First-launch flow: if no `BASE_URL` override is found, prompt user for their AgentOS instance URL. Persist to secure-store. Validate by calling `/healthz` before saving. Reject non-HTTPS URLs except `http://localhost:*` for dev.

### 5.9 Expo config

`mobile/app.json`:

```json
{
  "expo": {
    "name": "AgentOS",
    "slug": "agentos",
    "scheme": "agentos",
    "owner": "YOUR_ORG",
    "version": "0.1.0",
    "orientation": "portrait",
    "ios": { "bundleIdentifier": "com.yourorg.agentos" },
    "android": { "package": "com.yourorg.agentos" },
    "plugins": [
      "expo-router", "expo-secure-store", "expo-auth-session", "expo-notifications"
    ],
    "extra": { "eas": { "projectId": "TBD" } }
  }
}
```

## Files changed

| File | Change |
|------|--------|
| `mobile/` | new project (Expo) |
| `mobile/app/_layout.tsx` | root layout w/ auth gate |
| `mobile/app/(auth)/login.tsx` | login screen |
| `mobile/app/(main)/_layout.tsx` | tab nav |
| `mobile/app/(main)/{chat,tasks,pipelines,approvals,settings}.tsx` | tab screens (placeholders except settings) |
| `mobile/src/api/{client,queries,generated}.ts` | HTTP + codegen |
| `mobile/src/auth/{store,pkce,hooks}.ts` | auth module |
| `mobile/src/config.ts` | env config + URL trust-on-first-use |
| `mobile/app.json`, `mobile/tsconfig.json`, `mobile/tailwind.config.js` | project config |
| `.github/workflows/mobile.yml` | new — `npm ci && npx tsc --noEmit && npx jest` |

## Dependencies

- [[02-mobile-oauth2-auth-layer]] — the auth endpoints
- [[04-mobile-api-surface-audit]] — generated client source

## Test plan

- Unit: `generatePkce` — verifier base64url round-trip OK; SHA-256 challenge matches server-side computation in a fixture.
- Unit: `apiFetch` — 401 triggers single refresh attempt, subsequent requests use new token.
- Unit: auth store hydration returns `hydrated=true` even when no session exists.
- E2E (Maestro): launch app → login flow using a pre-seeded test user against a local test server → assert tab nav visible.
- Typecheck: `tsc --noEmit` clean.
- iOS + Android simulator: manual smoke test.

## Verification

```bash
cd mobile
npm ci
npx tsc --noEmit
npx expo prebuild --clean   # optional: verify native projects generate
npx expo start              # smoke in simulator
```

## Related

- [[Mobile App Plan]]
- [[02-mobile-oauth2-auth-layer]]
- [[04-mobile-api-surface-audit]]
- [[06-agent-chat-screen-sse]] — first real screen built on this scaffold
