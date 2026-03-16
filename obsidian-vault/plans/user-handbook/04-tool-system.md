---
title: Handbook Tool System
tags:
  - docs
  - tools
  - v3
  - plan
date: 2026-03-13
status: planned
effort: 4h
priority: high
---

# Handbook Tool System

> Write the Tool System chapter (built-in tools, manifests, trust tiers, signing) and the WASM Tools Development chapter (writing, compiling, installing WASM tools and using the SDK).

---

## Why This Subtask
Tools are the "programs" of AgentOS. Users need to understand the built-in tools, how to install community tools, the trust tier security model, how to sign tool manifests, and how to develop custom WASM tools. The existing tools guide (`docs/guide/05-tools-guide.md`) covers 8 built-in tools and basic WASM development but is missing the trust tier system, Ed25519 signing workflow, and 6+ additional tools added in V3.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Built-in tools documented | 8 (file-reader, file-writer, memory-search, memory-write, data-parser, shell-exec, agent-message, task-delegate) | All tools: + log-reader, http-client, network-monitor, process-manager, sys-monitor, hardware-info, archival-insert, archival-search, memory-block-* |
| Trust tier system | Not documented for users | Full section: Core, Verified, Community, Blocked tiers with behavior |
| Signing workflow | Not documented for users | End-to-end: keygen, sign, verify with examples |
| SDK macros | Not documented | `#[tool]` attribute macro with example |
| WASM development | Basic example | Full workflow: Rust + Python examples, manifest format, testing, installation |

---

## What to Do

### 1. Write `07-Tool System.md`

Read these source files for ground truth:
- `docs/guide/05-tools-guide.md` -- existing tools content
- `crates/agentos-tools/src/lib.rs` -- tool module exports
- `crates/agentos-tools/src/traits.rs` -- `AgentTool` trait definition
- `crates/agentos-tools/src/runner.rs` -- `ToolRunner` and execution logic
- `crates/agentos-tools/src/file_reader.rs` -- FileReader tool
- `crates/agentos-tools/src/file_writer.rs` -- FileWriter tool
- `crates/agentos-tools/src/memory_search.rs` -- MemorySearch tool
- `crates/agentos-tools/src/memory_write.rs` -- MemoryWrite tool
- `crates/agentos-tools/src/data_parser.rs` -- DataParser tool
- `crates/agentos-tools/src/shell_exec.rs` -- ShellExec tool
- `crates/agentos-tools/src/agent_message.rs` -- AgentMessage tool
- `crates/agentos-tools/src/task_delegate.rs` -- TaskDelegate tool
- `crates/agentos-tools/src/log_reader.rs` -- LogReader tool
- `crates/agentos-tools/src/http_client.rs` -- HttpClient tool
- `crates/agentos-tools/src/network_monitor.rs` -- NetworkMonitor tool
- `crates/agentos-tools/src/process_manager.rs` -- ProcessManager tool
- `crates/agentos-tools/src/sys_monitor.rs` -- SysMonitor tool
- `crates/agentos-tools/src/hardware_info.rs` -- HardwareInfo tool
- `crates/agentos-tools/src/archival_insert.rs` -- ArchivalInsert tool
- `crates/agentos-tools/src/archival_search.rs` -- ArchivalSearch tool
- `crates/agentos-tools/src/memory_block_read.rs` -- MemoryBlockRead tool
- `crates/agentos-tools/src/memory_block_write.rs` -- MemoryBlockWrite tool
- `crates/agentos-tools/src/memory_block_list.rs` -- MemoryBlockList tool
- `crates/agentos-tools/src/memory_block_delete.rs` -- MemoryBlockDelete tool
- `crates/agentos-tools/src/signing.rs` -- Ed25519 signing functions
- `crates/agentos-types/src/tool.rs` -- `ToolManifest`, `TrustTier` types
- `crates/agentos-kernel/src/tool_registry.rs` -- registration logic, trust tier enforcement
- `crates/agentos-cli/src/commands/tool.rs` -- CLI tool commands

The chapter must include:

**Section: How Tools Work**
- Intent flow: LLM declares intent -> kernel matches tool -> capability check -> sandboxed execution -> result injection
- Tool result wrapping format: `[TOOL_RESULT: name] { ... } [/TOOL_RESULT]`

**Section: Built-in Tools Reference**
For each built-in tool, document:
- Name
- Description (one sentence)
- Required permissions
- Input format (JSON keys)
- Output format
- Sandbox restrictions (network, fs_write)

Tools to document:
1. `file-reader` -- Read files from data directory
2. `file-writer` -- Write files to data directory
3. `memory-search` -- Search semantic memory
4. `memory-write` -- Write to semantic memory
5. `data-parser` -- Parse JSON/CSV data
6. `shell-exec` -- Execute shell commands (bwrap sandboxed)
7. `agent-message` -- Send message to another agent
8. `task-delegate` -- Delegate subtask to another agent
9. `log-reader` -- Read system/application logs
10. `http-client` -- Make HTTP requests
11. `network-monitor` -- Monitor network connections
12. `process-manager` -- List/manage system processes
13. `sys-monitor` -- System resource monitoring
14. `hardware-info` -- Query hardware information
15. `archival-insert` -- Insert into archival memory
16. `archival-search` -- Search archival memory
17. `memory-block-read` -- Read a memory block
18. `memory-block-write` -- Write a memory block
19. `memory-block-list` -- List memory blocks
20. `memory-block-delete` -- Delete a memory block

**Section: Tool Manifests**
- TOML manifest format with annotated example
- Every manifest field explained

**Section: Trust Tiers**
- `Core` -- distribution-trusted, no runtime signature check, loaded from `tools/core/`
- `Verified` -- signed with Ed25519, signature verified on install
- `Community` -- signed with Ed25519, signature verified on install
- `Blocked` -- hard-rejected by kernel

**Section: Tool Signing**
- `agentctl tool keygen` -- generate Ed25519 keypair
- `agentctl tool sign --manifest path --key keypair.json` -- sign a manifest
- `agentctl tool verify path` -- verify signature
- End-to-end workflow example

**Section: Installing and Removing Tools**
- `agentctl tool install <manifest>`
- `agentctl tool list`
- `agentctl tool remove <name>`

**Section: Tool Sandboxing**
- Native Rust tools: in-process, path traversal prevention
- WASM tools: Wasmtime isolation, epoch interruption, memory limits
- Shell-exec: bwrap namespace isolation, seccomp-BPF

### 2. Write `17-WASM Tools Development.md`

Read these source files:
- `docs/guide/05-tools-guide.md` -- existing WASM tool examples
- `crates/agentos-wasm/src/lib.rs` -- WASM runtime implementation
- `crates/agentos-tools/src/runner.rs` -- how ToolRunner dispatches to WASM
- `crates/agentos-sdk/src/lib.rs` -- SDK re-exports
- `crates/agentos-sdk-macros/src/lib.rs` -- `#[tool]` proc macro

The chapter must include:
- **WASM Tool Protocol** -- stdin JSON input, `AGENTOS_OUTPUT_FILE` output, exit code, stderr logging
- **Rust WASM tool** -- complete example with `wasm32-wasip1` target
- **Python WASM tool** -- complete example with py2wasm compilation
- **Tool manifest for WASM** -- full annotated manifest with `[executor]` section
- **Wasmtime sandbox guarantees** -- capability isolation, epoch interruption, memory limits, cleanup
- **SDK `#[tool]` macro** -- how to use the proc macro for native Rust tools (not WASM)
- **Testing tools locally** -- how to test before installing
- **Publishing workflow** -- sign, install, verify

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/07-Tool System.md` | Create new |
| `obsidian-vault/reference/handbook/17-WASM Tools Development.md` | Create new |

---

## Prerequisites
[[01-foundation-chapters]] must be complete (architecture context needed).

---

## Test Plan
- Both files exist
- Tool System chapter lists all built-in tools (check count >= 20 tool names)
- Trust tier section covers all 4 tiers
- Signing workflow has keygen/sign/verify examples
- WASM chapter has both Rust and Python examples
- `AgentTool` trait definition is shown in the Tool System chapter

---

## Verification
```bash
test -f obsidian-vault/reference/handbook/07-Tool\ System.md
test -f obsidian-vault/reference/handbook/17-WASM\ Tools\ Development.md

# All trust tiers documented
grep -c "Core\|Verified\|Community\|Blocked" obsidian-vault/reference/handbook/07-Tool\ System.md
# Should be >= 4

# WASM chapter has examples
grep -c "wasm32-wasip1\|py2wasm\|AGENTOS_OUTPUT_FILE" obsidian-vault/reference/handbook/17-WASM\ Tools\ Development.md
# Should be >= 3
```
