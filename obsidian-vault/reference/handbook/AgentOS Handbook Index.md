---
title: AgentOS Handbook Index
tags:
  - docs
  - handbook
date: 2026-03-17
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
| 04 | [[04-CLI Reference Complete]] | All 18 `agentctl` command groups with flags, arguments, and examples |
| 05 | [[05-Agent Management]] | Agent lifecycle, messaging, groups, identity keys, and agent registry |
| 06 | [[06-Task System]] | Task routing, lifecycle states, background tasks, and scheduled tasks |
| 07 | [[07-Tool System]] | Built-in tools, manifests, trust tiers (Core/Verified/Community/Blocked), signing |
| 08 | [[08-Security Model]] | 7 defense layers, capability tokens, permission enforcement, injection scanner, risk levels |
| 09 | [[09-Secrets and Vault]] | AES-256-GCM encrypted vault, secret scopes, rotation, lockdown mode |
| 10 | [[10-Memory System]] | 4 memory tiers, automatic extraction, consolidation, context budget management |
| 11 | [[11-Pipeline and Workflows]] | Multi-step YAML pipelines, step dependencies, failure handling, pipeline CLI |
| 12 | [[12-Event System]] | Event types, subscriptions, filter predicates, event-triggered tasks, throttle policy |
| 13 | [[13-Cost Tracking]] | Per-agent token costs, budget enforcement, model pricing table, cost CLI |
| 14 | [[14-Audit Log]] | 83+ event types, append-only SQLite chain, Merkle verification, export, snapshots |
| 15 | [[15-LLM Configuration]] | 5 provider adapters (Ollama, OpenAI, Anthropic, Gemini, Mock), endpoint resolution, env vars |
| 16 | [[16-Configuration Reference]] | Every config key in `config/default.toml` with type, default value, and description |
| 17 | [[17-WASM Tools Development]] | WASM execution protocol, Rust and Python examples, `#[tool]` SDK macro |
| 18 | [[18-Advanced Operations]] | HAL, resource locks, snapshots, escalation workflows, agent identity |
| 19 | [[19-Troubleshooting and FAQ]] | 33+ common errors with solutions, debug logging, health checks, platform notes |

---

## Quick Navigation

### By Role

**New to AgentOS?** Start at [[01-Introduction and Philosophy]] → [[02-Installation and First Run]] → [[03-Architecture Overview]].

**Operator running a deployment?** See [[04-CLI Reference Complete]], [[16-Configuration Reference]], and [[19-Troubleshooting and FAQ]].

**Developer building agents?** See [[05-Agent Management]], [[06-Task System]], [[07-Tool System]], and [[17-WASM Tools Development]].

**Security reviewer?** See [[08-Security Model]], [[09-Secrets and Vault]], and [[14-Audit Log]].

**Architect evaluating AgentOS?** See [[03-Architecture Overview]], [[10-Memory System]], [[11-Pipeline and Workflows]], and [[12-Event System]].

---

## System Components Cross-Reference

| Component | Primary Chapter | Related Chapters |
|-----------|----------------|-----------------|
| Kernel | [[03-Architecture Overview]] | [[06-Task System]], [[18-Advanced Operations]] |
| CLI (`agentctl`) | [[04-CLI Reference Complete]] | All chapters |
| Agents | [[05-Agent Management]] | [[06-Task System]], [[08-Security Model]] |
| Tasks | [[06-Task System]] | [[11-Pipeline and Workflows]], [[12-Event System]] |
| Tools | [[07-Tool System]] | [[17-WASM Tools Development]], [[08-Security Model]] |
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
| HAL | [[18-Advanced Operations]] | [[03-Architecture Overview]] |
| Troubleshooting | [[19-Troubleshooting and FAQ]] | [[14-Audit Log]], [[04-CLI Reference Complete]] |
