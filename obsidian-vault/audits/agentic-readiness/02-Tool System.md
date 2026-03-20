---
title: "Audit #2: Tool System"
tags:
  - audit
  - tools
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 3h
priority: critical
---

# Audit #2: Tool System

> Evaluating the tools I have access to, how I discover them, how they execute, and whether they're production-ready for autonomous agent workflows.

---

## Scope

- `crates/agentos-tools/` — 40+ tool implementations, traits, runner, loader, sanitizer, signing
- `crates/agentos-kernel/src/tool_registry.rs` — registry management
- `tools/core/*.toml` — 40+ TOML manifests

As an AI agent, tools are my **programs**. Every action I take in the world goes through a tool. The quality of the tool system directly determines what I can accomplish.

---

## Verdict: STRONG — with important usability gaps for agentic workflows

The tool system is well-architected: strong security, Ed25519 signing, defense-in-depth permission checks, proper sandboxing. The `agent-manual` tool is **excellent** — it's my primary way to learn the OS. However, several tools have limitations that would frustrate autonomous operation.

---

## Findings

### 1. AgentTool Trait — SOLID

**What works well:**
- Clean trait: `name()`, `execute(payload, context)`, `required_permissions()`.
- `ToolExecutionContext` provides everything I need: data_dir, IDs, permissions, vault, HAL, file locks, agent/task registries.
- Async execution via `#[async_trait]` — tools can do I/O without blocking.
- `ToolRunner::execute()` does defense-in-depth permission verification on top of kernel's pre-check.
- Execution timing is logged — useful for performance analysis.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 1 | No tool cancellation — once `execute()` starts, there's no way to cancel it mid-execution | High | Long-running tools (web-fetch, shell-exec) can't be interrupted |
| 2 | `ToolExecutionContext` has 5 `Option` fields — tools must handle `None` for vault, HAL, file_lock_registry, agent_registry, task_registry | Medium | Boilerplate error handling in every tool that needs these |
| 3 | No tool versioning at runtime — `ToolRunner` stores tools by name only, no way to have multiple versions | Low | Can't test new tool versions alongside old ones |

### 2. ToolRunner — GOOD

**What works well:**
- Registers 40+ tools at startup with shared memory stores (semantic, episodic, procedural).
- Graceful fallback: HttpClientTool and WebFetch log errors instead of crashing if init fails.
- `register_agent_manual()` is called after tool registry is loaded — agent-manual gets accurate tool list.
- `list_tools()` and `get_required_permissions()` for discovery.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 4 | `register_memory_tools()` also registers ALL non-memory tools — misleading name | Low | Code clarity issue, not runtime impact |
| 5 | No hot-reload — adding/removing tools requires restart | Medium | Can't install new tools at runtime |
| 6 | If `HttpClientTool::new()` fails, I silently lose HTTP capability with only a tracing::error | Medium | I won't know HTTP is unavailable until I try to use it |

### 3. File Operations — EXCELLENT

**file-reader:**
- Path traversal prevention via canonicalization + `starts_with()` data_dir check.
- 10 MiB size guard prevents OOM.
- Line-based pagination with offset/limit — I can read large files incrementally.
- Directory listing mode with sorted output.
- Write lock checking — won't read while another agent is writing.

**file-editor:**
- Exact string replacement model (old_text → new_text) — same as my primary interface.
- Atomic write via tmp + rename — no partial writes on crash.
- Write lock acquisition across the full read-modify-write cycle.
- Multi-byte UTF-8 safe truncation.
- Uniqueness enforcement: old_text must appear exactly once — prevents ambiguous edits.
- 10 MiB size guard consistent with file-reader.

**file-glob, file-grep, file-delete, file-move, file-diff:**
- Complete file operations suite matching my expected capabilities.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 7 | All file operations are relative to `data_dir` — I cannot access files outside this directory, even with permission | High | Severely limits my ability to work on real projects |
| 8 | `file-reader` default limit is 500 lines — not documented in the tool's response | Low | I may not realize I'm seeing truncated output |
| 9 | No binary file support — `read_to_string()` fails on binary files | Medium | Can't read images, compiled files, etc. |
| 10 | No file-create tool — I must use file-writer with `mode: "create_only"` | Low | Extra cognitive overhead |

### 4. Shell Execution — STRONG SECURITY

**What works well:**
- **Mandatory bwrap sandboxing** — refuses to run without bubblewrap.
- Network isolation by default — must explicitly opt in with `allow_network`.
- Read-only root filesystem, writable only in data_dir.
- Sensitive directories hidden (/root, /etc, /var, /home).
- Fresh /tmp, /dev, /proc per execution.
- Timeout enforcement.
- Output truncation at 50KB.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 11 | `--unshare-all` includes PID namespace — I can't inspect processes outside the sandbox | Low | Expected for security |
| 12 | No working directory parameter — always runs in data_dir | Medium | Can't run commands in project directories |
| 13 | `/etc` is hidden — commands that need `/etc/resolv.conf` (like curl, apt) fail when network is allowed | High | Network-enabled commands break because DNS resolution needs /etc/resolv.conf |
| 14 | No stdin support — interactive commands can't work | Low | Expected limitation |

### 5. HTTP Client — COMPREHENSIVE

**What works well:**
- Multi-layer SSRF protection:
  - IP-based checks (loopback, private, unspecified, multicast).
  - Hostname checks (localhost, .local).
  - DNS pre-resolution — catches hostnames that resolve to private IPs.
  - Redirect SSRF checks on every hop.
- Secret header injection via vault ($SECRET_NAME pattern).
- SSE streaming with structured event parsing.
- Download-to-file with 100MB limit and automatic cleanup on overflow.
- Multipart upload support.
- JSON auto-parsing when content-type matches.
- Binary response base64 encoding.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 15 | DNS pre-resolution is TOCTOU vulnerable — hostname could resolve differently between check and request | Low | Mitigated by the redirect SSRF checks |
| 16 | No retry logic — a single timeout kills the request | Medium | Flaky network connections cause failures |
| 17 | Max 10MB response body for standard mode — API responses from large datasets get truncated | Medium | May need to use save_to for large responses |
| 18 | Secret headers log the header *name* (via tracing::info) — not the value, which is correct, but the name itself may be sensitive | Low | Minor info leak |

### 6. Agent Manual — EXCELLENT

This tool is the **single most important tool for agentic readiness**. It's how I learn the OS.

**What works well:**
- 12 queryable sections covering every major system.
- Tool discovery: list all tools, get detailed per-tool docs.
- Permission model documentation with resource classes and matching rules.
- Memory tier documentation with tool mappings.
- Event system documentation with all 50+ event types organized by category.
- Error recovery guidance — tells me what to do for each error type.
- Agent coordination patterns — step-by-step delegation workflow.
- Procedural memory documentation — how to record and reuse procedures.
- Feedback mechanism documentation.
- No permissions required — I can always access the manual.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 19 | No `input_schema` documentation for most tools — the `tool-detail` section shows `null` for input_schema | High | I must guess the payload format for each tool |
| 20 | No section for "budget and cost" — missing budget management docs | Medium | I can't learn about cost tracking |
| 21 | No section for "pipelines" — missing workflow docs | Medium | I can't learn about pipeline system |
| 22 | `commands` section mixes tool-accessible and kernel-only commands without clear distinction | Medium | I might try to call kernel-internal commands as tools |

### 7. Think Tool — PERFECT

Zero-permission reasoning tool. No side effects. Captured in audit log. This is exactly what an agent needs for deliberation steps. No issues.

### 8. Datetime Tool — SIMPLE AND CORRECT

Returns current datetime. No issues.

### 9. Tool Signing & Trust — SOLID

**What works well:**
- Ed25519 signing with deterministic canonical JSON payload.
- Four trust tiers with clear policies.
- CRL (Certificate Revocation List) support — revoked keys block tools.
- `verify_manifest_with_crl()` chain: CRL check → trust tier check → signature verification.
- Comprehensive test coverage (10 tests including tamper detection).

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 23 | No automatic CRL update mechanism — CRL must be manually loaded from file | Medium | Revoked tools stay active until CRL is refreshed |
| 24 | `Core` tier bypasses all signature checks — a compromised core TOML is trusted unconditionally | Low | Expected for distribution trust model |

### 10. Tool Registry — GOOD

**What works well:**
- Name-indexed lookup: `get_by_name()` — I find tools by name.
- Lifecycle event notifications: Install, Remove, ChecksumMismatch.
- `tools_for_prompt()` generates system prompt tool listing.
- Tests cover registration, removal, lifecycle events, and bad signatures.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 25 | `tools_for_prompt()` returns `"- name : description"` — no input schema, no permissions | High | I can see tools but don't know how to call them without consulting agent-manual |
| 26 | No search by capability — I can't ask "which tools can write files?" | Medium | Must enumerate all tools to find one by capability |
| 27 | No tool dependency tracking — if tool A depends on tool B, this isn't modeled | Low | Future concern |

### 11. Output Sanitization — CORRECT

- Escapes `[TOOL_RESULT`, `[SYSTEM`, `[AGENT_DIRECTORY`, `[CONTEXT SUMMARY` patterns.
- Prevents prompt injection via tool output.
- UTF-8-safe truncation.
- Tests cover injection, delimiter escaping, and truncation.

No issues. Well-implemented.

### 12. TOML Manifests — COMPLETE

All 40+ tools have manifests with:
- Name, version, description, author, trust_tier.
- Capability permissions.
- Sandbox settings (network, fs_write, GPU, memory, CPU limits).
- Intent schema names.

**Issues:**

| # | Issue | Severity | Impact on Agent |
|---|-------|----------|-----------------|
| 28 | No `input_schema` JSON Schema in any TOML manifest — this field is always absent | High | No schema validation for tool inputs |
| 29 | Manifest `permissions` use colon notation ("fs.user_data:r") but `PermissionSet` uses struct-based rwx — mapping is undocumented | Medium | Format mismatch between manifest and runtime |

---

## Critical Gaps for Pure Agentic Workflow

### Gap A: No Input Schema Documentation

The most critical gap. As an LLM, I construct tool payloads as JSON. Without documented input schemas:
- I must learn each tool's expected fields by trial and error.
- Schema validation errors are my only guide.
- The `agent-manual` tool-detail section shows `null` for input_schema.

**Recommendation:** Add `[input_schema]` to every TOML manifest defining JSON Schema for the input. Wire it into `agent-manual`'s tool-detail section.

### Gap B: Data Directory Confinement

All file operations are confined to `data_dir` (default: `/tmp/agentos/data`). In a real agentic workflow where I'm working on a user's project:
- I can't read the project's source code.
- I can't write test files.
- I can't run builds.

This makes AgentOS suitable for sandboxed data tasks but **not for general software engineering workflows**.

**Recommendation:** Add configurable workspace directories that can be mapped into the agent's accessible paths, with appropriate permission grants.

### Gap C: No Tool Timeout/Cancellation

If `http-client` hangs on a slow server or `shell-exec` runs a command that takes too long:
- The tool-level timeout (30s default) helps but can't be interrupted from outside.
- No `CancellationToken` is passed to tool execution.
- The kernel's task timeout may fire, but the tool's Future will keep running until it completes or the tokio runtime is dropped.

**Recommendation:** Pass a `CancellationToken` in `ToolExecutionContext` and check it in long-running tools.

---

## Test Coverage Assessment

| Module | Unit Tests | Integration Tests | Coverage Quality |
|--------|-----------|-------------------|-----------------|
| runner.rs | 0 | via e2e | **Missing unit tests** |
| signing.rs | 10 | 0 | Excellent — covers all trust tiers, tampering, CRL |
| sanitize.rs | 5 | 0 | Good — injection, truncation |
| tool_registry.rs | 7 | via e2e | Good — lifecycle events, CRL |
| agent_manual.rs | 18 | 0 | Excellent — all sections tested |
| http_client.rs | 11 | 0 | Good — SSE parsing, path normalization |
| file_editor.rs | 3 | 0 | Adequate — UTF-8 truncation only |
| shell_exec.rs | 0 | via e2e | **Missing** |
| think.rs | 2 | 0 | Complete |
| file_reader.rs | 0 | via e2e | **Missing** — pagination untested |

---

## Score

| Criterion | Score (1-5) | Notes |
|-----------|------------|-------|
| Completeness | 4.0 | 40+ tools covering all major operations |
| Correctness | 4.5 | Path traversal, SSRF, atomic writes all solid |
| Agent Ergonomics | 2.5 | No input schemas, data_dir confinement, missing error context |
| Security | 4.8 | Ed25519, bwrap, SSRF, defense-in-depth — excellent |
| Documentation (agent-manual) | 4.0 | Comprehensive but missing input schemas |
| **Overall** | **3.9/5** | Strong security and architecture, weak discoverability |
