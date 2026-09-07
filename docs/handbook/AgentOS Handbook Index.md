---
title: AgentOS Handbook Index
tags:
  - docs
  - handbook
date: 2026-04-30
status: complete
---

# AgentOS User Handbook

> The complete guide to installing, configuring, and operating AgentOS — an LLM-native operating system for AI agents.

---

## Chapters

| # | Chapter | Summary |
|---|---------|---------|
| 01 | [[01-Introduction and Philosophy]] | What AgentOS is, core principles, the Linux analogy — LLMs as CPU, tools as programs, intent as syscall |
| 02 | [[02-Installation and First Run]] | Prerequisites, building from source, configuration, first kernel boot |
| 03 | [[03-Architecture Overview]] | System architecture, crate dependency graph, the intent flow from CLI to tool execution |
| 04 | [[04-CLI Reference Complete]] | All 38 `agentos` command groups with flags, arguments, and examples — includes `team`, `skill`, `mcp` (attach/detach/oauth-store/a2a-*), `a2a` (card/discover/delegate/tasks), `workspace`, `provider`, `plugin`, `onboard`, `doctor`, `config`, `init`, plus `start`, `stop`, `scratchpad`, `healthz`, `log`, `notifications`, `channel`, and `web` |
| 05 | [[05-Agent Management]] | Agent lifecycle, messaging, groups, identity keys, agent registry, multi-agent coordination (sub-agents, teams) |
| 06 | [[06-Task System]] | Task routing, lifecycle states, background tasks, scheduled tasks, sub-agent tasks (parent_task_id, spawn_depth, team coordinator), checkpointing & resume |
| 07 | [[07-Tool System]] | All 132 built-in tools across 16 domains — file I/O, memory tiers, scratchpad, multi-agent coordination, task management, HAL device tools, agent inbox (6 tools), schedule inspection (4), tool discovery/pagination (4), host introspection (4), manifests, trust tiers, signing |
| 08 | [[08-Security Model]] | 8 core defense layers (+ KMC), capability tokens, permission enforcement, injection scanner, risk levels, hooks (`AuditHook`, `ApprovalHook`) |
| 09 | [[09-Secrets and Vault]] | AES-256-GCM encrypted vault, secret scopes, rotation, lockdown mode, OAuth credential storage |
| 10 | [[10-Memory System]] | 4 memory tiers, automatic extraction, consolidation, context budget management |
| 11 | [[11-Pipeline and Workflows]] | Multi-step YAML pipelines, wave-based parallel execution, step dependencies, failure handling, budget enforcement, variable sanitization |
| 12 | [[12-Event System]] | Event types, subscriptions, filter predicates, event-triggered tasks, throttle policy |
| 13 | [[13-Cost Tracking]] | Per-agent token costs, budget enforcement, model pricing table, cost CLI |
| 14 | [[14-Audit Log]] | 146 event types across 37 categories — task lifecycle, MCP, OAuth, containers, IoT device twins, checkpoint recovery, append-only SQLite chain, Merkle verification, export, snapshots |
| 15 | [[15-LLM Configuration]] | Four native adapters (Ollama, OpenAI, Anthropic, Gemini) plus the 20-entry provider catalog (DeepSeek, Groq, Mistral, xAI, Cohere, Cerebras, OpenRouter, Together, Fireworks, NVIDIA, Hyperbolic, Azure, and more) — connecting, env vars, FallbackAdapter, RetryPolicy, CircuitBreaker |
| 16 | [[16-Configuration Reference]] | Every config key in `config/default.toml` with type, default value, and description |
| 17 | [[17-WASM Tools Development]] | WASM execution protocol, Rust and Python examples, `#[tool]` SDK macro |
| 18 | [[18-Advanced Operations]] | HAL (20 drivers incl. audio, bluetooth, webcam, printer, display, raw USB, GPU, MQTT, Home Assistant, mounts, sockets, open-files, services), device twins & safety engine, consent store, resource locks, snapshots, escalation, identity |
| 19 | [[19-Troubleshooting and FAQ]] | 33+ common errors with solutions, debug logging, health checks, platform notes |
| 20 | [[20-LLM Agent Testing]] | `agent-tester` binary — LLM-driven scenario testing, feedback protocol, report format, CI integration |
| 21 | [[21-User Notifications and Channels]] | Agent-to-operator messaging — `notify-user`, `ask-user`, delivery channels (Telegram, ntfy, email), notification inbox CLI |
| 22 | [[22-MCP Integration]] | Bidirectional MCP bridge — boot-time servers, runtime `mcp attach`/`detach`, persisted across restarts, OAuth credential lifecycle, A2A (Agent-to-Agent) protocol, security gate (injection scanning + rate limit) |
| 23 | [[23-REST API Reference]] | 35+ REST endpoints under `/api/v1/*` plus the OpenAI-compatible `/v1/chat/completions` SSE endpoint — auth, permissions, request/response shapes, rate limiting, error codes |
| 24 | [[24-WebSocket Guide]] | Real-time event subscriptions, chat streaming (token-level `TextChunk` events), task control — frame protocol, available channels, reconnection patterns |
| 25 | [[25-API Authentication and Keys]] | API key lifecycle (create, scope, expire, revoke), HMAC validation internals, WebSocket auth, CSRF middleware, security best practices |
| 26 | [[26-Channel Adapters]] | All 10 bidirectional messaging adapters — Discord, Telegram, Slack, Matrix, Mattermost, Teams, LINE, WhatsApp, Email, Webhook — pairing manager, health monitor, retry with backoff |
| 27 | [[27-Kernel Mediated Capabilities]] | 5 managed capability domains (env, storage, proc, net, build) with 17 bridge tools — policy engine, dynamic grants, 7-layer SSRF defense, structured output parsing, per-agent isolation |
| 28 | [[28-Agent Inbox and Notifications]] | Agent async notification inbox + agent-to-agent message inbox — SQLite persistence, idempotent writes, capacity-based eviction, system prompt segment design, 6 tools |

---

## Quick Navigation

### By Role

**New to AgentOS?** Start at [[01-Introduction and Philosophy]] → [[02-Installation and First Run]] → [[03-Architecture Overview]].

**Operator running a deployment?** See [[04-CLI Reference Complete]], [[16-Configuration Reference]], and [[19-Troubleshooting and FAQ]].

**Developer building agents?** See [[05-Agent Management]] (including multi-agent coordination and teams), [[06-Task System]], [[07-Tool System]], [[17-WASM Tools Development]], [[21-User Notifications and Channels]], [[22-MCP Integration]], [[26-Channel Adapters]], and [[28-Agent Inbox and Notifications]].

**Testing and evaluating AgentOS?** See [[20-LLM Agent Testing]].

**Security reviewer?** See [[08-Security Model]], [[09-Secrets and Vault]], [[14-Audit Log]], and [[25-API Authentication and Keys]].

**Integrating via REST API?** Start at [[23-REST API Reference]] → [[25-API Authentication and Keys]] → [[24-WebSocket Guide]].

**Architect evaluating AgentOS?** See [[03-Architecture Overview]], [[10-Memory System]], [[11-Pipeline and Workflows]], and [[12-Event System]].

---

## System Components Cross-Reference

| Component | Primary Chapter | Related Chapters |
|-----------|----------------|-----------------|
| Kernel | [[03-Architecture Overview]] | [[06-Task System]], [[18-Advanced Operations]] |
| CLI (`agentos`) | [[04-CLI Reference Complete]] | All chapters |
| Agents | [[05-Agent Management]] | [[06-Task System]], [[07-Tool System]], [[08-Security Model]] |
| Tasks | [[06-Task System]] | [[11-Pipeline and Workflows]], [[12-Event System]] |
| Tools | [[07-Tool System]] | [[05-Agent Management]], [[17-WASM Tools Development]], [[08-Security Model]] |
| Security | [[08-Security Model]] | [[09-Secrets and Vault]], [[14-Audit Log]] |
| Vault | [[09-Secrets and Vault]] | [[08-Security Model]] |
| Memory | [[10-Memory System]] | [[06-Task System]], [[03-Architecture Overview]] |
| Pipelines | [[11-Pipeline and Workflows]] | [[06-Task System]], [[12-Event System]] |
| Events | [[12-Event System]] | [[11-Pipeline and Workflows]], [[06-Task System]] |
| Cost Tracking | [[13-Cost Tracking]] | [[06-Task System]], [[14-Audit Log]] |
| Audit Log | [[14-Audit Log]] | [[08-Security Model]], [[19-Troubleshooting and FAQ]] |
| LLM | [[15-LLM Configuration]] | [[03-Architecture Overview]], [[06-Task System]] |
| Config | [[16-Configuration Reference]] | [[02-Installation and First Run]] |
| WASM Tools | [[17-WASM Tools Development]] | [[07-Tool System]], [[08-Security Model]] |
| HAL | [[18-Advanced Operations]] | [[03-Architecture Overview]], [[04-CLI Reference Complete]] |
| Troubleshooting | [[19-Troubleshooting and FAQ]] | [[14-Audit Log]], [[04-CLI Reference Complete]] |
| LLM Agent Testing | [[20-LLM Agent Testing]] | [[15-LLM Configuration]], [[07-Tool System]], [[08-Security Model]] |
| Notifications | [[21-User Notifications and Channels]] | [[07-Tool System]], [[08-Security Model]], [[09-Secrets and Vault]] |
| MCP | [[22-MCP Integration]] | [[07-Tool System]], [[08-Security Model]], [[04-CLI Reference Complete]] |
| REST API | [[23-REST API Reference]] | [[25-API Authentication and Keys]], [[24-WebSocket Guide]], [[08-Security Model]] |
| WebSocket | [[24-WebSocket Guide]] | [[23-REST API Reference]], [[12-Event System]], [[21-User Notifications and Channels]] |
| API Keys | [[25-API Authentication and Keys]] | [[23-REST API Reference]], [[08-Security Model]], [[09-Secrets and Vault]] |
| Channel Adapters | [[26-Channel Adapters]] | [[21-User Notifications and Channels]], [[23-REST API Reference]], [[08-Security Model]], [[09-Secrets and Vault]] |
| Agent Inbox | [[28-Agent Inbox and Notifications]] | [[07-Tool System]], [[06-Task System]], [[12-Event System]], [[21-User Notifications and Channels]] |
