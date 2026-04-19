---
title: Phase 10 — Distribution & Release
tags:
  - mobile
  - eas
  - app-store
  - release
  - phase-10
date: 2026-04-19
status: planned
effort: 3d
priority: high
---

# Phase 10 — Distribution & Release

> Ship it: EAS Build pipeline, TestFlight + Google Play Internal Testing tracks, store listings, privacy policy, crash reporting, OTA update channel, and a smoke-test checklist for every release.

---

## Why this phase

An unreleased mobile app is worth zero. This phase turns the codebase into something an end user can install. It also bakes in the ops loop (crash reports → fixes → OTA updates) so we can iterate post-launch.

## Current → Target state

**Current:** Mobile app runs in Expo Go / dev client only.

**Target:**
- `eas.json` with three build profiles: `development`, `preview` (internal APK + TestFlight), `production`.
- Apple Developer + Google Play Console accounts registered; signing configured in EAS.
- First build submitted to TestFlight + Google Play Internal Testing.
- Crash + error reporting via Sentry (`sentry-expo`).
- OTA updates via `expo-updates` on a `production` branch — ship JS-only fixes without store review.
- Store listings: screenshots, description, keywords, privacy policy URL, support URL.
- Release checklist `mobile/RELEASE.md` + GitHub Actions workflow `.github/workflows/mobile-release.yml`.
- Versioning policy: `semver` for user-visible app version; `buildNumber` / `versionCode` auto-incremented by EAS.

## Detailed subtasks

### 10.1 EAS configuration

File: `mobile/eas.json`.

```json
{
  "cli": { "version": ">= 12.0.0", "appVersionSource": "remote" },
  "build": {
    "development": {
      "developmentClient": true,
      "distribution": "internal",
      "channel": "development"
    },
    "preview": {
      "distribution": "internal",
      "ios": { "simulator": false },
      "channel": "preview",
      "env": { "EXPO_PUBLIC_AGENTOS_BASE_URL": "https://staging.agentos.example.com" }
    },
    "production": {
      "autoIncrement": true,
      "channel": "production",
      "env": { "EXPO_PUBLIC_AGENTOS_BASE_URL": "https://agentos.example.com" }
    }
  },
  "submit": {
    "production": {
      "ios":     { "ascAppId": "TBD", "appleTeamId": "TBD" },
      "android": { "serviceAccountKeyPath": "./android-service-account.json", "track": "internal" }
    }
  }
}
```

Secrets (`APPLE_ID`, `ASC_API_KEY`, Google service account JSON) configured via EAS Secrets, NEVER committed.

### 10.2 App icons + splash

- Icon set from Figma → `mobile/assets/icon.png` (1024×1024, no alpha).
- Adaptive Android icon (`assets/adaptive-icon.png` + background color).
- Splash screen via `expo-splash-screen`, dark mode variant.
- `expo-asset` prebundle to avoid first-run download.

### 10.3 Crash + error reporting

Add `sentry-expo`:

```bash
npx expo install sentry-expo @sentry/react-native
```

`mobile/src/monitoring/sentry.ts`:

```ts
import * as Sentry from 'sentry-expo';

Sentry.init({
  dsn: process.env.EXPO_PUBLIC_SENTRY_DSN,
  enableInExpoDevelopment: false,
  debug: __DEV__,
  tracesSampleRate: 0.1,
  beforeSend: (event) => {
    // Strip auth tokens and push tokens from breadcrumbs
    return sanitizeEvent(event);
  },
});
```

Wrap root `<App>` with `Sentry.Native.wrap`. Add a manual test `throw new Error('sentry smoke')` in dev-only settings.

### 10.4 OTA update pipeline

`expo-updates` config in `app.json`:

```json
"updates": {
  "enabled": true,
  "fallbackToCacheTimeout": 0,
  "url": "https://u.expo.dev/TBD",
  "checkAutomatically": "ON_LOAD"
},
"runtimeVersion": { "policy": "appVersion" }
```

`eas update --branch production --message "fix X"` for JS-only hotfixes. Runtime version bumps whenever native deps change (signals the user to update from the store).

### 10.5 Privacy policy + store listings

Privacy policy MUST disclose:
- Authentication tokens stored locally in Keychain/Keystore
- Push tokens shared with Apple APNs / Google FCM via Expo Push Service
- Crash reports sent to Sentry (optional opt-out toggle in Settings)
- All chat/task content travels to **the user's self-hosted AgentOS instance** — not to us
- No advertising, no analytics tracking

Host at `https://agentos.example.com/privacy` (or GitHub Pages).

Store listings:
- Screenshots (6.7" iPhone + 6.5" iPhone + 5 Android device sizes)
- Short description (80 chars)
- Long description mentioning self-hosted architecture
- Keywords: AI agent, automation, workflow, approvals
- Support URL → GitHub issues

### 10.6 App review prep

Expected Apple review friction:
1. **"What backend does this connect to?"** — explain self-hosted; provide a review-only test AgentOS instance with pre-loaded demo data. Include the URL + test account in App Review Information.
2. **"How is data encrypted?"** — TLS in transit, Keychain at rest, SQLCipher on server.
3. **Background push usage** — justify with escalation workflow screenshot.

Google Play is less strict; focus on Data Safety form — mark all categories honestly.

### 10.7 GitHub Actions release workflow

File: `.github/workflows/mobile-release.yml` (new).

```yaml
name: mobile-release
on:
  workflow_dispatch:
    inputs:
      profile: { type: choice, options: [preview, production], default: preview }

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 20, cache: npm, cache-dependency-path: mobile/package-lock.json }
      - working-directory: mobile
        run: npm ci && npx tsc --noEmit && npx jest
      - uses: expo/expo-github-action@v8
        with: { eas-version: latest, token: ${{ secrets.EXPO_TOKEN }} }
      - working-directory: mobile
        run: eas build --profile ${{ inputs.profile }} --platform all --non-interactive
      - if: inputs.profile == 'production'
        working-directory: mobile
        run: eas submit --profile production --platform all --non-interactive
```

### 10.8 Release checklist

File: `mobile/RELEASE.md`.

```
## Before building
- [ ] `cargo test --workspace` green on main
- [ ] `mobile/` `tsc --noEmit` + `jest` green
- [ ] OpenAPI regenerated; no drift (`git diff mobile/src/api/generated.ts`)
- [ ] CHANGELOG updated; app version bumped in app.json
- [ ] Privacy policy up-to-date for any new data category

## Build
- [ ] EAS build profile matches release type
- [ ] Build artifact signed with production cert (prod only)

## QA smoke (every release)
- [ ] Login + refresh works
- [ ] Chat streaming works on flaky network (Network Link Conditioner)
- [ ] Task create/run/cancel
- [ ] Pipeline template run
- [ ] Push arrives + Approve action works from lock screen
- [ ] Logout + re-login clears all state
- [ ] Deep link agentos://callback handled

## Submit
- [ ] TestFlight: external testers notified
- [ ] Play Internal: testers notified
- [ ] Monitor Sentry for 48h before widening rollout
```

### 10.9 Versioning

- `app.json.version`: `0.1.0` for first internal release. Use semver.
- `iOS buildNumber` and `Android versionCode`: auto-incremented via `eas.json.cli.appVersionSource: remote`.
- Runtime version bumps only when native deps change — otherwise OTA can patch.

### 10.10 Post-launch loop

- Weekly cadence: review Sentry errors, cut an OTA hotfix if needed.
- Monthly cadence: ship a store build with accumulated changes.
- Every release: update `MEMORY.md` entry and `CHANGELOG.md`.

## Files changed

| File | Change |
|------|--------|
| `mobile/eas.json` | new |
| `mobile/assets/{icon,adaptive-icon,splash}.png` | new |
| `mobile/app.json` | icons, splash, updates, sentry |
| `mobile/src/monitoring/sentry.ts` | new |
| `mobile/RELEASE.md` | new — release checklist |
| `mobile/PRIVACY.md` (or hosted at /privacy) | new |
| `.github/workflows/mobile-release.yml` | new |
| `mobile/package.json` | add `sentry-expo`, `expo-updates`, `expo-splash-screen`, `expo-asset` |

## Dependencies

- All prior phases (5-9) implemented and working.

## Test plan

- EAS development build boots on device.
- Preview build distributed via TestFlight + internal AB — at least 3 manual testers complete the QA smoke checklist.
- OTA update: push a no-op JS change, verify it rolls out to preview channel within 5 min.
- Sentry receives a forced test error with scrubbed auth headers (verify no `Authorization` strings in the event).
- App Review sandbox: test account logs in, runs demo pipeline, approves an escalation.

## Verification

```bash
cd mobile
npm ci
npx tsc --noEmit
npx jest
eas build --profile preview --platform ios --non-interactive --no-wait
# Inspect Sentry DSN reachability:
curl -o /dev/null -sw "%{http_code}\n" "$EXPO_PUBLIC_SENTRY_DSN"
```

## Related

- [[Mobile App Plan]]
- [[05-mobile-app-scaffold-and-auth]]
- [[09-approval-workflow-ux]] — the review-anchor feature
