---
title: Intent Processing Flow
tags: [flow, intent, tools]
---

# Intent Processing Flow

How tool calls are validated and executed within the kernel.

## Flow Diagram

```
LLM Response
    │
    ▼
┌──────────────────┐
│ Tool Call Parser  │  Extract tool_id, payload, intent_type
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ Capability       │  1. Verify HMAC signature
│ Validator        │  2. Check token expiry
│                  │  3. Tool in allowed_tools?
│                  │  4. Intent in allowed_intents?
│                  │  5. Permission bits sufficient?
└────────┬─────────┘
         │
         ▼ (valid)
┌──────────────────┐
│ Tool Registry    │  Look up tool by name
│ Lookup           │
└────────┬─────────┘
         │
         ├─── Inline tool ──► ToolRunner.execute()
         │                        │
         └─── WASM tool ───► WasmToolExecutor
                                  │
                                  ▼
                          ┌──────────────┐
                          │ Sandbox      │  seccomp-BPF filter
                          │ (if needed)  │  bwrap isolation
                          └──────┬───────┘
                                 │
                                 ▼
                          Tool Result (JSON)
                                 │
                                 ▼
                          ┌──────────────┐
                          │ Context Push │  push_tool_result()
                          └──────┬───────┘
                                 │
                                 ▼
                          Back to LLM for next iteration
```

## Validation Details

### Step 1: Signature Check
```
HMAC-SHA256(token_data, kernel_signing_key) == token.signature
```
If fails → `PermissionDenied` error, audit log entry with `Security` severity.

### Step 2: Expiry Check
```
now < token.expires_at
```
If expired → `TokenExpired` event, new token may be issued.

### Step 3: Tool Authorization
```
token.allowed_tools.contains(tool_id)
```
Ensures the agent is only using tools approved for this task.

### Step 4: Intent Authorization
```
token.allowed_intents.contains(intent_type)
```
Restricts which operation types (Read, Write, Execute, etc.) are permitted.

### Step 5: Permission Check
```
token.permissions.check(resource, operation) == true
```
Verifies the specific resource permission (e.g., `fs.user_data:r`).

## Tool Execution

### Inline Tools
- Direct function call within the kernel process
- Path validation for file operations
- Permission checks within the tool itself
- Returns `serde_json::Value`

### WASM Tools
- Pre-compiled Wasmtime module
- JSON input via stdin
- Output written to `$AGENTOS_OUTPUT_FILE`
- Epoch-based CPU time limiting
- Isolated from host filesystem

### Shell Execution
- Wrapped in `bwrap` (bubblewrap)
- Read-only root mount
- Data directory read-write
- Sensitive paths hidden
- seccomp-BPF syscall filter applied
- Null byte injection blocked

## Audit Trail

Every step generates audit entries:
- `ToolExecutionStarted` → when tool begins
- `ToolExecutionCompleted` → on success
- `ToolExecutionFailed` → on error
- `PermissionDenied` → on authorization failure
