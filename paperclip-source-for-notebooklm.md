# Paperclip — The Control Plane for AI Agents (Reference Source)

> Curated reference compiled from the official GitHub repo (paperclipai/paperclip),
> paperclipai.net, and launch coverage (March 2026). Use as a NotebookLM source for
> comparing Paperclip against AgentOS.

## What Paperclip Is

Paperclip is an open-source **Node.js server + React UI** that orchestrates a team of
AI agents to run a business. It positions itself as "the control plane for AI agents":
"Hire AI employees, set goals, automate jobs and your business runs itself."

- Launched **March 2, 2026**; crossed ~38,000 GitHub stars within its first month.
- Tech stack: **Node.js 20+, pnpm 9.15+, PostgreSQL** (embedded locally, external in
  production), **React** front end, **TypeScript** (~98% of codebase), Vitest + Playwright.
- Self-hosted; supports local / LAN / Tailnet binding.

Paperclip explicitly is **NOT**: a chatbot UI, an agent development framework, a visual
workflow builder, a prompt manager, or a single-agent tool. It does **not build agents** —
it orchestrates agents you already have.

## The CEO Agent and Hierarchy

- A **CEO agent** is the planning layer: it reads the product backlog / high-level goals,
  breaks them into strategic objectives and smaller tasks, and decides which specialist
  agents to assign. The CEO delegates to department heads, who further delegate to
  specialized workers.
- A standard "AI company" setup uses four agents: **CEO, researcher, marketer, designer.**
- Agents have **roles, titles, reporting lines, permissions, and budgets** — a real org chart.
- Delegation flows **both up and down** the org chart. Tasks carry **full goal ancestry**
  so every agent sees the "why," not just a title.
- "Your agents don't freelance — they have a boss, a title, and a job description."

## What Counts as an "Agent"

Agent-agnostic: "Any agent, any runtime, one org chart. **If it can receive a heartbeat,
it's hired.**" Supported runtimes:
- Claude Code sessions
- Codex agents
- CLI agents (Cursor, bash, Gemini)
- HTTP / webhook bots (e.g. OpenClaw)
- External adapter plugins, Python scripts, shell commands

Agents are onboarded with **adapter examples** defining how they receive tasks and report back.

## Heartbeat Execution Model

- "Agents **wake on a schedule**, check work, and act."
- **DB-backed wakeup queue with coalescing.**
- Each run does: budget checks → workspace resolution → secret injection → skill loading →
  adapter invocation.
- Runs produce structured logs, cost events, session state, and audit trails.
- **Recovery handles orphaned runs automatically.**
- **Persistent context:** agents resume the same task context across heartbeats instead of
  restarting from scratch.

## Work Management

- Unit of work is an **Issue**, carrying company/project/goal/parent links.
- **Atomic checkout with execution locks** → "no double-work and no lost context."
- First-class **blocker dependencies**, comments, documents, attachments, work products,
  labels, inbox state.

## Workspace Isolation

- Project workspaces + **isolated execution workspaces (git worktrees, operator branches)**.
- Runtime services: dev servers, preview URLs.

## Budgets & Cost Control

- "Monthly budgets per agent. **When they hit the limit, they stop.**"
- Token/cost tracking by company, agent, project, goal, issue, provider, and model.
- Scoped budget policies with warning thresholds and **hard stops**.
- Overspend **pauses agents and cancels queued work**.

## Governance & Human Approval

- "You sit at the top. Approve hires, review strategy, override decisions. Pause. Resume.
  Override. Reassign. Terminate."
- **Board approval workflows**; execution policies with review/approval stages.
- Agents **cannot hire new agents without your approval**; the CEO's initial strategic plan
  requires sign-off. "Nothing ships without your sign-off."

## Audit & Observability

- "Every conversation traced. Every decision explained. Full tool-call tracing and
  **immutable audit log.**"
- Tracks mutating actions, heartbeat state changes, cost events, approvals, comments, work
  products. "Every mutating request is traced to an actor."

## Security Model

- Two deployment modes: **trusted local** or **authenticated**.
- **Agent API keys, short-lived run JWTs, company memberships.**
- Encrypted secret storage with scoped access.
- **True multi-company isolation** — every entity is company-scoped; one deployment runs many
  companies with separate data and audit trails. Companies export/import as templates with
  automatic secret scrubbing.

## Roadmap

- Done: plugin system, OpenClaw integration, org import/export, scheduled routines, budgeting,
  multi-user support.
- In progress: cloud agents, artifacts, knowledge systems, **self-organization**.

## Canonical Sources
- GitHub: https://github.com/paperclipai/paperclip
- Site: https://paperclipai.net/ and https://paperclip.ing/
- Launch writeups: theaienterprise.io, mindstudio.ai, zeabur.com (deploy guide)
