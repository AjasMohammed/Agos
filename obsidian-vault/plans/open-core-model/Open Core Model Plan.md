---
title: AgentOS Open Core Model
tags:
  - business
  - monetization
  - strategy
date: 2026-04-15
status: planned
effort: ongoing
priority: high
---

# AgentOS Open Core Model

> Defines the boundary between free open-source features and paid enterprise capabilities, enabling community adoption while creating a sustainable revenue path.

---

## Why Open Core

Open core is the proven monetization model for infrastructure software (HashiCorp, Elastic, GitLab, Grafana). The pattern:

- **Open source core** — enough to be genuinely useful, drives community adoption and trust
- **Enterprise tier** — features that security/compliance-sensitive buyers require and are willing to pay for
- **Cloud tier** — hosted version that removes operational burden, appeals to teams that want the value without the ops

The key discipline: the open source tier must be *truly useful*, not crippled. If users feel nickeled-and-dimed, they leave. The enterprise tier should contain features that only matter at organizational scale, not features individuals need.

---

## Tier Definitions

### Community (Free, Apache 2.0)

Everything in the current codebase. This includes:

| Feature | Rationale |
|---------|-----------|
| Full inference kernel | Core value — must be free |
| All LLM adapters (20+ providers) | Breadth drives adoption |
| All built-in tools (30+) | Core utility |
| WASM + seccomp sandboxing | Security basics must be free |
| Capability tokens + permission system | Security basics must be free |
| Encrypted vault (AES-256-GCM) | Security basics must be free |
| Audit trail (129 event types, hash-chain) | Core trust feature — must be free |
| 3-tier memory (episodic/semantic/procedural) | Core agent capability |
| Task checkpointing | Reliability basics |
| Web UI (dashboard, tasks, audit) | Discovery and usability |
| REST API (OpenAI-compatible) | Integration breadth |
| Channel adapters (Slack, Discord, Telegram…) | Integration breadth |
| Multi-agent teams + A2A delegation | Core agent capability |
| Pipeline orchestration | Core workflow capability |
| Plugin marketplace | Ecosystem growth |
| CLI (all 27 commands) | Full self-service |
| Docker + docker-compose | Standard deployment |
| MCP support | Ecosystem compatibility |
| Hardware abstraction layer | Differentiation feature |
| Cost tracking (per-task attribution) | Core observability |
| OpenTelemetry tracing | Core observability |

### Enterprise (Paid — $2,000–$10,000/month)

Features that only matter at organizational scale or in regulated industries:

| Feature | Why It's Enterprise |
|---------|-------------------|
| **Compliance report export** (PDF + JSON) | Regulatory requirement for audits; requires organizational governance |
| **SSO / SAML integration** | Enterprise procurement requirement |
| **Multi-tenant isolation** | Multiple teams with hard data boundaries |
| **Advanced RBAC with org hierarchy** | Department → team → agent permission inheritance |
| **Audit log retention policies** | Configurable legal hold, data residency |
| **SLA + dedicated support** | Organizational risk requirement |
| **Air-gapped deployment package** | Defense/healthcare requirement |
| **Custom compliance profiles** | SOC2, HIPAA, ISO27001 report templates |
| **Approval workflow integrations** | PagerDuty, ServiceNow, Jira for escalations |
| **Enterprise SSE dashboard** | Real-time multi-agent monitoring at scale |
| **Priority model routing** | Preferred providers, cost caps per department |
| **Quarterly security reviews** | Enterprise security team requirement |

### AgentOS Cloud (Hosted — $49–$299/month per seat)

Hosted version of the Community tier. No ops required:

| Tier | Price | Limits |
|------|-------|--------|
| Developer | Free | 1 agent, 100 tasks/month, community support |
| Team | $49/seat/month | 10 agents, 5,000 tasks/month |
| Professional | $149/seat/month | Unlimited agents, 50,000 tasks/month |
| Enterprise Cloud | Custom | Unlimited + Enterprise features + SLA |

---

## The Open Core Discipline

**Rules to never break:**

1. **Never gate security basics.** Capability tokens, vault, sandboxing, and the audit trail are always free. If users have to pay to be secure, they won't trust you.

2. **Never gate individual productivity.** A solo developer should get full value from the free tier. Enterprise tier is about organizational features, not individual features.

3. **Never surprise with paywalls.** Document the boundary clearly. Users should know what's free before they invest in building on it.

4. **Compliance report is the anchor.** The `agentos audit report` command (already built) is the bridge between free and paid. Community users get the raw audit data; Enterprise users get formatted reports with custom templates, PDF export, and compliance mapping.

---

## Implementation Roadmap

| Phase | Deliverable | Target |
|-------|------------|--------|
| 1 | Open source release + Apache 2.0 license in place | v0.1.0 |
| 2 | Enterprise waitlist landing page | v0.1.0 |
| 3 | SSO/SAML integration (okta, azure AD) | v0.2.0 |
| 4 | Compliance report templates (SOC2, HIPAA) | v0.2.0 |
| 5 | AgentOS Cloud alpha (waitlist) | v0.3.0 |
| 6 | Multi-tenant kernel isolation | v0.3.0 |
| 7 | Enterprise support tier + SLA | v1.0.0 |

---

## Related

- [[First Deployment Readiness Plan]]
- [[Strategic Roadmap]]
