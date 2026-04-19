---
title: Mobile App Data Flow
tags:
  - mobile
  - flow
  - plan
date: 2026-04-19
status: complete
effort: doc-only
priority: high
---

# Mobile App Data Flow

> Data and control flow diagrams covering auth, chat, task execution, pipeline creation, and push-based approvals for the mobile client ↔ cloud AgentOS.

---

## 1. Authentication (OAuth2 + PKCE)

```mermaid
sequenceDiagram
    participant App as Mobile App
    participant Browser as In-App Browser
    participant API as agentos-api
    participant Vault as agentos-vault
    participant DB as auth.db

    App->>App: Generate code_verifier + code_challenge (S256)
    App->>Browser: Open /v1/auth/authorize?client_id=mobile&code_challenge=...&redirect=agentos://callback
    Browser->>API: GET /v1/auth/authorize
    API->>Browser: Render login form (password or webauthn)
    Browser->>API: POST credentials
    API->>Vault: Verify credential
    API->>DB: Insert auth_code row (code, verifier_hash, user_id, exp)
    API->>Browser: 302 agentos://callback?code=...
    Browser->>App: Deep link with code
    App->>API: POST /v1/auth/token (code + code_verifier)
    API->>DB: Verify code_verifier matches challenge
    API->>App: {access_token (15m JWT), refresh_token (30d), expires_in}
    App->>App: Store tokens in expo-secure-store (Keychain/Keystore)
```

Access-token refresh (silent, on 401 or near-expiry):

```mermaid
sequenceDiagram
    App->>API: POST /v1/auth/refresh (refresh_token)
    API->>DB: Validate + rotate (old token revoked, new token issued)
    API->>App: {access_token, refresh_token}
```

## 2. Chat (SSE)

```mermaid
sequenceDiagram
    participant App
    participant API as agentos-api
    participant Kernel as agentos-kernel
    participant LLM

    App->>API: POST /v1/chat/completions (stream=true, Authorization: Bearer JWT)
    API->>Kernel: Submit chat task via internal channel
    Kernel->>LLM: infer_stream(ctx, tools)
    loop streaming tokens
        LLM-->>Kernel: token chunk
        Kernel-->>API: SSE event {delta}
        API-->>App: data: {"choices":[{"delta":{"content":"..."}}]}
    end
    LLM-->>Kernel: done
    Kernel-->>API: SSE event [DONE]
    API-->>App: data: [DONE]
    Note over App: If connection drops, reconnect with Last-Event-ID header
```

## 3. Task execution + checkpoint monitoring

```mermaid
flowchart LR
    App -->|POST /v1/tasks| API
    API -->|SubmitTask bus msg| Kernel
    Kernel -->|spawn| TaskExec[task_executor]
    TaskExec -->|writes| Checkpoints[(checkpoint.db)]
    TaskExec -->|audit events| Audit[(audit.db)]
    App <-.->|GET /v1/tasks/:id SSE| API
    API -.->|poll + stream| Kernel
    App -->|GET /v1/tasks/:id/checkpoints| API
    API --> Kernel
    Kernel --> Checkpoints
    App -->|POST /v1/tasks/:id/resume| API
```

## 4. Pipeline creation + run

```mermaid
sequenceDiagram
    participant App
    participant API
    participant Kernel as agentos-kernel
    participant Pipe as agentos-pipeline

    App->>API: POST /v1/pipelines {name, steps:[...]} (JSON)
    API->>Kernel: CreatePipeline cmd
    Kernel->>Pipe: store definition
    Kernel-->>API: {pipeline_id}
    API-->>App: 201 {id, name, ...}

    App->>API: POST /v1/pipelines/:id/run
    API->>Kernel: RunPipeline cmd
    Kernel->>Pipe: execute
    loop each step
        Pipe->>Kernel: submit AgentTask
        Kernel-->>Pipe: result
    end
    Pipe-->>Kernel: PipelineRunComplete
    App<-.->API: GET /v1/pipelines/:id/runs/:run_id (SSE for live progress)
```

## 5. Approval workflow (the flagship mobile UX)

```mermaid
sequenceDiagram
    participant Kernel as agentos-kernel
    participant Hook as ApprovalHook
    participant Ch as MobilePushAdapter
    participant Expo as Expo Push / APNs+FCM
    participant App as Mobile App
    participant API as agentos-api

    Kernel->>Hook: ToolPre event (high-risk tool)
    Hook->>Kernel: Create PendingEscalation(auto_action=Deny, exp=5min)
    Kernel->>Ch: emit MobileNotification{escalation_id, tool, preview}
    Ch->>Expo: send high-priority push
    Expo->>App: silent + visible notification (actionable: Approve / Deny)
    App->>App: display sheet with tool + input preview
    App->>API: POST /v1/escalations/:id/resolve {decision: approve|deny}
    API->>Kernel: ResolveEscalation cmd
    Kernel->>Kernel: Resume / abort tool call
    Kernel-->>API: 200 OK
    API-->>App: confirmation
```

Fallback path (approval not delivered within 5 min):

```mermaid
flowchart TD
    Expire[PendingEscalation TTL expires] --> Sweep[TimeoutChecker.sweep_expired]
    Sweep --> Deny[Apply auto_action = Deny]
    Deny --> Abort[Tool call aborted with EscalationDenied]
    Abort --> Audit[Audit event EscalationAutoDenied]
    Audit --> Push[MobilePushAdapter sends 'missed approval' push]
```

## 6. Device registration

```mermaid
sequenceDiagram
    App->>App: request push permission
    App->>Expo: getExpoPushToken()
    Expo-->>App: ExponentPushToken[xxx]
    App->>API: POST /v1/devices {platform, push_token, name, model}
    API->>DB: insert device row (user_id, push_token, platform, created_at)
    API-->>App: {device_id}
    Note over App: Store device_id in secure-store for future unregister
```

## Related

- [[Mobile App Plan]]
- [[Mobile App Research]]
- [[01-cloud-deployment-foundation]]
- [[02-mobile-oauth2-auth-layer]]
- [[03-device-registration-and-push-relay]]
- [[09-approval-workflow-ux]]
