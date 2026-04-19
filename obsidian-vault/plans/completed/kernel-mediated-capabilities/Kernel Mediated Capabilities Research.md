---
title: Kernel Mediated Capabilities Research
tags:
  - kernel
  - security
  - capabilities
  - research
date: 2026-04-12
status: complete
effort: 1d
priority: high
---

# Kernel Mediated Capabilities Research

> Research synthesis on capability-based security, mediated system access patterns, and how competing agent systems handle the autonomy vs security trade-off.

---

## The Problem Space

AI agents need system-level powers to be useful for real-world tasks (software engineering, devops, data processing). But granting raw OS access is dangerous — prompt injection, hallucination, and multi-agent trust escalation can cause unbounded damage.

The question: **How do you give agents full system powers without raw system access?**

---

## Industry Approaches (2025-2026)

### OpenHands / Devin Model: Docker Isolation

**Architecture:** Each agent session runs in a Docker container with full root access. The container is the blast radius boundary.

**Strengths:**
- Agents can do everything: install packages, run builds, manage processes, access networks
- Simple mental model: "full access inside the box"
- Proven in production (OpenHands, Devin, Codex)

**Weaknesses:**
- Requires Docker — doesn't work on bare metal
- No per-action audit trail (just shell history)
- No policy control (all-or-nothing access)
- Single-agent only (no multi-agent isolation within container)
- Prompt injection = full container compromise
- No structured output (agents parse raw stdout)

**Source:** OpenHands SDK paper (arXiv:2511.03690) — event-stream architecture with Docker-backed sessions

### WASI / Wassette Model: Capability Handles

**Architecture:** Programs run in WebAssembly sandbox. Each WASI capability (filesystem, network, clock) must be explicitly granted by the host runtime. Programs cannot access anything they weren't given a handle to.

**Strengths:**
- Capability-based by design — no ambient authority
- Portable across platforms
- Formally verifiable memory safety
- Fine-grained: grant access to specific directories, specific network hosts
- Microsoft's Wassette (Aug 2025) applies this specifically to AI agent tool execution

**Weaknesses:**
- WASI ecosystem still maturing (async I/O in WASI 0.3 landed mid-2025)
- Not all tools can be compiled to WASM (native dependencies, system tools)
- Performance overhead for I/O-heavy workloads
- No process management primitives in WASI yet

**Source:** Wassette announcement (rawkode.academy), WASI component model spec, Wasmtime security docs

### NanoClaw Model: Capability-Based Agent Platform

**Architecture:** Applies capability-based security (inspired by seL4, Fuchsia/Zircon) directly to AI agents. Agents receive capability tokens that grant specific powers. Isolation over trust.

**Key insight from NanoClaw:** "Capability-based security models have seen renewed interest... NanoClaw's application of these principles to AI agents represents a practical implementation of ideas that have long been considered theoretically sound but operationally difficult."

**Source:** WebProNews analysis (2026)

### Android / Fuchsia Model: System Services

**Architecture:** Apps never access hardware directly. The OS provides system services (LocationManager, CameraManager, PackageManager) that mediate all access. Apps request permissions, the system grants/denies, and every access is audited.

**Relevance to AgentOS:**
- AgentOS already has HAL drivers (system services equivalent)
- The missing piece: HAL drivers are mostly read-only; they need to become action-capable
- Permission model exists (CapabilityToken) but needs runtime negotiation

### AgenticOS 2026 Workshop Findings

The SOSP AgenticOS 2026 workshop identifies the core challenge: "AI agents challenge traditional OS abstractions — processes, threads, files, sockets — that were never designed for dynamic, semantically rich, adaptive agent workloads."

**Key themes:**
- Need for new isolation models beyond process/container boundaries
- "Governed execution" — agents perform tasks with policy enforcement, persistent memory, audit trails
- Integration middleware that mediates between agents and legacy systems through well-defined APIs

**Source:** os-for-agent.github.io, EasyChair CFP AgenticOS2026

---

## What AgentOS Already Has

| Mechanism | Status | Reusable For KMC? |
|-----------|--------|-------------------|
| HAL drivers (16) | Mature, some action-capable | Yes — extend from monitoring to management |
| WASI/Wasmtime | Functional, CPU/memory limits | Yes — WASI capability grants per-tool |
| bwrap sandbox | Complete, mandatory for shell | Yes — use for managed build/process isolation |
| CapabilityToken (HMAC-SHA256) | Production-ready | Yes — extend with dynamic grants |
| PermissionSet (wildcard, prefix, deny) | Production-ready | Yes — add per-destination network policy |
| PendingEscalation | Complete, auto-deny on expiry | Yes — dynamic capability negotiation |
| DeviceAccessGate | Per-device quarantine | Yes — model for all capability approval |
| Seccomp-BPF | Linux, ~100 syscalls | Yes — per-provider syscall profiles |
| Landlock | Linux, write restriction | Yes — expand to zone-based access |
| Audit log (83+ event types) | Append-only, hash-chained | Yes — add per-resource events |

---

## Key Design Insights from Research

### 1. Mediation > Restriction

Every successful platform follows this pattern: don't block access, mediate it. Android doesn't prevent apps from using the camera — it provides a CameraManager that enforces permissions, audits usage, and provides structured results.

**Implication:** AgentOS should not block agents from installing packages. It should provide a PackageProvider that validates the request, checks the allowlist, installs into a scoped workspace, and returns structured results.

### 2. Capability Handles, Not Ambient Authority

WASI's core insight: programs should receive explicit handles to the resources they need, not inherit ambient authority from the user. A WASI program can only access files it was given a directory handle to.

**Implication:** Each agent task should receive capability handles for specific resources (this workspace, this network destination, this package set) — not broad "file system access" or "network access."

### 3. No Docker Dependency

OpenHands requires Docker because Docker is the isolation boundary. But many AgentOS users install directly on their system. KMC must use kernel-native isolation (Landlock, cgroups, namespaces) that works on bare metal.

### 4. Structured I/O is Non-Negotiable

OpenHands agents parse raw stdout from shell commands. This is fragile, lossy, and wastes LLM tokens. Capability providers should return structured JSON that the agent can reason about directly.

### 5. Dynamic Negotiation Beats Static Grants

Static capability grants (decided at task start) force operators to over-provision or under-provision. Agents that can request capabilities at runtime — subject to policy and approval — are both more capable and more secure.

### 6. Default Allowlists Reduce Operator Burden

If every capability requires manual configuration, operators won't configure anything and agents stay blocked. Ship curated default allowlists per ecosystem (popular Python packages, standard Node modules, common build tools).

---

## Related

- [[Kernel Mediated Capabilities Plan]]
- [[Kernel Mediated Capabilities Data Flow]]
