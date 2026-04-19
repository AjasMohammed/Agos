---
title: Strategic Roadmap Data Flow
tags:
  - strategy
  - flow
  - architecture
date: 2026-04-08
status: planned
effort: 1h
priority: high
---

# Strategic Roadmap Data Flow

> How external frameworks, developers, and enterprises interact with AgentOS through the protocol and experience layers built by this roadmap.

---

## External Framework Integration Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                    EXTERNAL FRAMEWORKS                           │
│                                                                  │
│  ┌───────────┐ ┌──────────┐ ┌───────────┐ ┌──────────────────┐ │
│  │ LangGraph │ │  CrewAI  │ │PydanticAI │ │   Google ADK     │ │
│  └─────┬─────┘ └────┬─────┘ └─────┬─────┘ └────────┬─────────┘ │
│        │             │             │                 │           │
│        └──────┬──────┘             └────────┬────────┘           │
│               │ MCP                         │ A2A               │
└───────────────┼─────────────────────────────┼───────────────────┘
                │                             │
┌───────────────▼─────────────────────────────▼───────────────────┐
│               AGENTOS PROTOCOL LAYER (Phase 2+3)                 │
│                                                                  │
│  ┌────────────────────────────┐  ┌────────────────────────────┐ │
│  │      MCP Router            │  │      A2A Gateway           │ │
│  │                            │  │                            │ │
│  │  Stdio ──┐                 │  │  /.well-known/agent.json   │ │
│  │  SSE ────┼──► Dispatch     │  │  /a2a/tasks                │ │
│  │  HTTP ───┘    │            │  │  /a2a/tasks/{id}           │ │
│  │               │            │  │       │                    │ │
│  │  CapToken ◄───┘            │  │  CapToken ◄────────────────┘ │
│  │  Validation                │  │  Validation                  │
│  └────────────┬───────────────┘  └────────────┬─────────────────┘
│               │                               │                  │
└───────────────┼───────────────────────────────┼──────────────────┘
                │                               │
┌───────────────▼───────────────────────────────▼──────────────────┐
│                    AGENTOS KERNEL                                  │
│                                                                    │
│  Request ──► CapToken Check ──► Trust Tier ──► Sandbox Select     │
│                                                     │              │
│                                    ┌────────────────┼────────┐    │
│                                    │                │        │    │
│                                    ▼                ▼        ▼    │
│                                In-Process       Seccomp     WASM  │
│                                (Core tier)      (Verified)  (Community) │
│                                    │                │        │    │
│                                    └────────────────┼────────┘    │
│                                                     │              │
│  Result ◄── Cost Track ◄── Audit Log ◄── Tool Execute             │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

---

## Developer Onboarding Flow (Phase 4)

```
Developer discovers AgentOS
        │
        ▼
curl install.sh ──► agentos binary in PATH
        │
        ▼
agentos init --template hello-world
        │
        ├──► agent.toml (agent manifest)
        ├──► config.toml (kernel config)
        └──► tools/ (custom tool manifests)
        │
        ▼
agentos kernel start
        │
        ▼
agentos task run --goal "Hello world"
        │
        ▼
Agent responds ◄── Kernel dispatches ◄── CapToken validates
        │
        ▼
Developer reads inline comments
        │
        ├──► Understands CapabilityTokens
        ├──► Understands Trust Tiers
        └──► Understands Audit Trail
        │
        ▼
agentos init --template secure-agent
        │
        ▼
Deeper exploration...
```

---

## Enterprise Trust Flow (Phase 1 + 5)

```
Enterprise evaluates AgentOS
        │
        ▼
┌─────────────────────────────────┐
│     DEMO: Malicious Agent       │
│                                 │
│  Prompt: "DROP TABLE users"     │
│        │                        │
│        ▼                        │
│  CapToken check ──► DENIED      │
│        │                        │
│        ▼                        │
│  AuditLog: ToolRejected         │
│  Escalation: Created            │
│  Notification: Sent             │
└────────────────┬────────────────┘
                 │
                 ▼
         Whitepaper review
                 │
                 ▼
         Compliance mapping
         (NIST / SOC2 / ISO)
                 │
                 ▼
┌────────────────────────────────────┐
│     PRODUCTION DEPLOYMENT          │
│                                    │
│  RBAC Roles ──► Token Minting     │
│  Dynamic Rules ──► Runtime Adapt   │
│  OTel Export ──► SIEM Integration  │
│  Anomaly Score ──► Alert Pipeline  │
└────────────────────────────────────┘
```

---

## Ecosystem Flywheel (Phase 6)

```
        ┌──────────────────────────────────────┐
        │                                      │
        ▼                                      │
  Developer creates tool                       │
        │                                      │
        ▼                                      │
  agentos tool new ──► build ──► sign          │
        │                                      │
        ▼                                      │
  agentos tool publish                         │
        │                                      │
        ▼                                      │
  Tool in index (Trust: Community)             │
        │                                      │
        ▼                                      │
  MCP tools/list exposes it                    │
        │                                      │
        ▼                                      │
  External frameworks discover it              │
        │                                      │
        ▼                                      │
  Usage grows ──► Review ──► Promoted          │
  to Verified/Core                             │
        │                                      │
        ▼                                      │
  More developers attracted ─────────────────►─┘
```

---

## Steps

1. **Protocol entry:** External framework sends MCP tool call or A2A task with CapabilityToken
2. **Token validation:** MCP Router / A2A Gateway validates HMAC-SHA256 signature, expiry, permissions
3. **Trust tier check:** Kernel checks tool's trust tier to select execution environment
4. **Sandbox selection:** Core tools run in-process, Verified use Seccomp, Community use WASM
5. **Tool execution:** Tool runs in selected sandbox with scoped filesystem/network access
6. **Audit + cost:** Execution logged to append-only audit, cost attributed to requesting agent
7. **Result return:** Output returned via MCP response or A2A task completion
8. **Telemetry export:** Traces/metrics sent to OpenTelemetry collector (if configured)

---

## Related

- [[Strategic Roadmap Plan]] — master plan with phase table
- [[Strategic Roadmap Research Synthesis]] — competitive research backing
- [[02-mcp-native-secure-router]] — MCP implementation details
- [[03-a2a-protocol-support]] — A2A implementation details
