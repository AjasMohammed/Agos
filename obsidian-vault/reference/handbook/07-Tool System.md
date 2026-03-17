---
title: Tool System
tags:
  - tools
  - handbook
  - reference
  - v3
date: 2026-03-17
status: complete
effort: 4h
priority: high
---

# Tool System

> Tools are the programs of AgentOS. They are sandboxed execution units that an agent calls by declaring intent — the kernel validates the request, checks permissions, runs the tool, and injects the result back into the agent's context window.

---

## How Tools Work

The execution path from LLM intent to tool result:

```
LLM declares intent
  → kernel matches tool by name
  → CapabilityToken validated against required PermissionSet
  → payload validated against tool's input_schema (if present)
  → tool executed in its sandbox (Rust in-process / WASM module / bwrap shell)
  → result injected into ContextWindow as:
    [TOOL_RESULT: <name>] { ... } [/TOOL_RESULT]
  → AuditLog entry written
```

The `AgentTool` trait every tool must implement:

```rust
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError>;
    fn required_permissions(&self) -> Vec<(String, PermissionOp)>;
}
```

At execution time the kernel constructs a `ToolExecutionContext` containing the task and agent IDs, the agent's verified `PermissionSet`, an optional reference to the `ProxyVault` (for secret injection), and an optional reference to the `HardwareAbstractionLayer`. Permission checks are enforced twice — once at the kernel router level and once inside `ToolRunner` as defence-in-depth. A tool cannot run unless both checks pass.

```rust
pub struct ToolExecutionContext {
    pub data_dir: PathBuf,      // agent's data directory
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub trace_id: TraceID,
    pub permissions: PermissionSet,
    pub vault: Option<Arc<ProxyVault>>,
    pub hal: Option<Arc<HardwareAbstractionLayer>>,
    pub file_lock_registry: Option<Arc<FileLockRegistry>>,
}
```

---

## Built-in Tools Reference

All built-in tools ship as `trust_tier = "core"` manifests in `tools/core/`. They are compiled into the kernel as Rust code — no external binary or WASM module is loaded.

### `file-reader`

Read files from the agent's data directory with line-based pagination and directory listing.

| | |
|---|---|
| **Permission** | `fs.user_data:r` |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `path` | string | Yes | — | Relative to data dir. Absolute paths are re-rooted inside data dir. |
| `mode` | string | No | `"read"` | `"read"` or `"list"` |
| `offset` | u64 | No | `0` | Start line for pagination |
| `limit` | u64 | No | `500` | Max lines to return; `0` = no cap |

**Output (read mode):**
```json
{
  "path": "notes.txt",
  "content": "...",
  "size_bytes": 1234,
  "total_lines": 42,
  "returned_lines": 42,
  "offset": 0,
  "has_more": false,
  "content_type": "text"
}
```

**Output (list mode):**
```json
{
  "path": ".",
  "mode": "list",
  "entries": [{ "name": "report.txt", "size_bytes": 512, "is_dir": false }],
  "count": 1
}
```

**Restrictions:** Path traversal denied — any resolved path outside `data_dir` returns `PermissionDenied`. Files larger than 10 MiB are rejected. Reads are blocked while another agent holds a write lock on the file (`FileLocked` error).

---

### `file-writer`

Write files to the agent's data directory.

| | |
|---|---|
| **Permission** | `fs.user_data:w` |
| **Network** | No |
| **fs_write** | Yes |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `path` | string | Yes | — | Relative to data dir |
| `content` | string | Yes | — | Content to write |
| `mode` | string | No | `"overwrite"` | `"overwrite"`, `"append"`, or `"create_only"` |
| `max_bytes` | u64 | No | — | Reject write if content exceeds this limit |

**Output:**
```json
{ "path": "out.txt", "mode": "overwrite", "bytes_written": 512 }
```

**Restrictions:** Same path traversal enforcement as `file-reader`. Parent directories are created automatically. `create_only` fails if the file already exists. Acquires a write lock for the duration of the write — concurrent readers get `FileLocked`.

---

### `memory-search`

Hybrid vector + full-text search across semantic or episodic memory.

| | |
|---|---|
| **Permission** | `memory.semantic:r` (semantic scope), `memory.episodic:r` (episodic global / cross-task) |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `query` | string | Yes | — | Natural-language search query |
| `scope` | string | No | `"semantic"` | `"semantic"` or `"episodic"` |
| `top_k` / `limit` | u64 | No | `5` | Max results; hard cap: 100 |
| `min_score` | f64 | No | `0.3` | Minimum relevance score (semantic only) |
| `global` | bool | No | `false` | Episodic: search across all agents and tasks |
| `since` | string | No | — | Episodic global: RFC3339 timestamp lower bound |
| `agent_id` | UUID string | No | — | Episodic global: filter by agent UUID |
| `task_id` | UUID string | No | — | Episodic: filter by task UUID |

**Output:**
```json
{
  "query": "revenue forecast",
  "results": [
    {
      "key": "q1-revenue",
      "content": "Q1 revenue was 2.5 million",
      "semantic_score": 0.87,
      "fts_score": 0.1,
      "rrf_score": 0.05,
      "tags": ["revenue"],
      "created_at": "2026-01-01T00:00:00Z",
      "scope": "semantic"
    }
  ],
  "count": 1
}
```

---

### `memory-write`

Write content to semantic or episodic memory.

| | |
|---|---|
| **Permission** | `memory.semantic:w` (semantic) or `memory.episodic:w` (episodic) |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `content` | string | Yes | — | Max 512 KiB |
| `scope` | string | No | `"semantic"` | `"semantic"` or `"episodic"` |
| `key` | string | No | auto-generated | Semantic: lookup key (first 6 words if omitted) |
| `tags` | array \| string | No | — | Semantic: array or comma-separated string |
| `summary` | string | No | — | Episodic: short one-line summary |

**Output:**
```json
{ "success": true, "scope": "semantic", "id": "uuid-...", "message": "Semantic memory entry stored with embedding" }
```

---

### `data-parser`

Parse JSON, CSV, or TOML text into structured JSON.

| | |
|---|---|
| **Permission** | None |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Notes |
|-----|------|----------|-------|
| `data` | string | Yes | Max 4 MiB |
| `format` | string | Yes | `"json"`, `"csv"`, or `"toml"` |

**Output:**
```json
{ "format": "csv", "parsed": { "headers": ["name","age"], "rows": [{"name":"Alice","age":"30"}], "row_count": 1 } }
```

CSV is capped at 50,000 rows. TOML is converted to JSON using standard key mapping.

---

### `shell-exec`

Execute a shell command inside a bwrap namespace sandbox.

| | |
|---|---|
| **Permission** | `process.exec:x`, `fs.user_data:w` |
| **Network** | No (opt-in) |
| **fs_write** | Yes (data dir only) |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `command` | string | Yes | — | Shell command (run via `sh -c`) |
| `timeout_secs` | u64 | No | `30` | Hard execution timeout |
| `allow_network` | bool | No | `false` | Pass `--share-net` to bwrap |

**Output:**
```json
{ "command": "ls -la", "exit_code": 0, "stdout": "...", "stderr": "", "success": true }
```

**Restrictions:** `bwrap` (bubblewrap) must be installed — the tool hard-fails without it. Root filesystem is read-only; only `data_dir` is writable. `/etc`, `/var`, `/root`, `/home` are hidden behind tmpfs. Fresh `/tmp`, `/dev`, `/proc` are provided. stdout and stderr are each truncated at 50,000 characters.

---

### `agent-message`

Send a message to another named agent.

| | |
|---|---|
| **Permission** | `agent.message:x` |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Notes |
|-----|------|----------|-------|
| `to` | string | Yes | Target agent name |
| `content` | string | Yes | Message text |

**Output:** Returns a `_kernel_action: "send_agent_message"` envelope; the kernel delivers the message.

---

### `task-delegate`

Delegate a subtask to another agent.

| | |
|---|---|
| **Permission** | `agent.delegate:x` |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `agent` | string | Yes | — | Target agent name |
| `task` | string | Yes | — | Task description |
| `priority` | u64 | No | `5` | Priority 0–10 |

**Output:** Returns a `_kernel_action: "delegate_task"` envelope processed by the kernel scheduler.

---

### `log-reader`

Read system and application logs via the Hardware Abstraction Layer.

| | |
|---|---|
| **Permission** | `fs.app_logs:r`, `fs.system_logs:r` |
| **Network** | No |
| **fs_write** | No |

The payload is forwarded to the HAL `log` query interface. The HAL resolves available log sources on the host (journald, syslog, application log files).

---

### `http-client`

Make outbound HTTP requests with automatic SSRF protection.

| | |
|---|---|
| **Permission** | `network.outbound:x` |
| **Network** | Yes |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `url` | string | Yes | — | Absolute URL |
| `method` | string | No | `"GET"` | GET, POST, PUT, PATCH, DELETE, HEAD |
| `headers` | object | No | — | String key-value pairs |
| `secret_headers` | object | No | — | Values use `$SECRET_NAME` syntax resolved from ProxyVault |
| `body` | any | No | — | Object/array → JSON body; string → raw body |
| `timeout_ms` | u64 | No | `10000` | Per-request timeout |

**Output:**
```json
{ "status": 200, "headers": { "content-type": "application/json" }, "body": {}, "latency_ms": 42, "truncated": false }
```

**Restrictions:** SSRF protection blocks loopback addresses, RFC1918 private IP ranges, link-local addresses, `localhost`, and `.local` hostnames. Response body is capped at 10 MiB (`truncated: true` flag set when exceeded). Redirects are not followed.

**Secret injection:** Headers in `secret_headers` use `$VAR_NAME` syntax:
```json
{ "secret_headers": { "Authorization": "Bearer $MY_API_TOKEN" } }
```
The kernel resolves `$MY_API_TOKEN` from the ProxyVault at runtime — the plaintext value is never serialized into the tool's input or the audit log.

---

### `network-monitor`

Monitor active network connections via the HAL.

| | |
|---|---|
| **Permission** | `network.logs:r` |
| **Network** | No |
| **fs_write** | No |

Payload forwarded to HAL `network` query interface. Returns active connections, listen sockets, and recent traffic statistics.

---

### `process-manager`

List or terminate system processes via the HAL.

| | |
|---|---|
| **Permission** | `process.list:r` (list), `process.kill:x` (kill) |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `action` | string | No | `"list"` | `"list"` or `"kill"` |

For `kill`, additional HAL-specific fields (e.g. `pid`) are required. Permissions are checked per-action: `list` requires `process.list:r`, `kill` requires `process.kill:x`.

---

### `sys-monitor`

Query current system resource usage (CPU, RAM, disk) via the HAL.

| | |
|---|---|
| **Permission** | `hardware.system:r` |
| **Network** | No |
| **fs_write** | No |

No required input fields. Returns a snapshot of CPU usage, memory usage, disk I/O, and load average.

---

### `hardware-info`

Query static hardware information via the HAL.

| | |
|---|---|
| **Permission** | `hardware.system:r` |
| **Network** | No |
| **fs_write** | No |

No required input fields. Returns CPU model, core count, total RAM, disk capacity, GPU presence.

---

### `archival-insert`

Insert a document into archival (semantic) memory with vector embedding.

| | |
|---|---|
| **Permission** | `memory.semantic:w` |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Default | Notes |
|-----|------|----------|---------|-------|
| `content` | string | Yes | — | Document text |
| `key` | string | No | `"archival-note"` | Lookup key |
| `tags` | array | No | `[]` | Metadata tags |

**Output:** `{ "success": true, "id": "uuid-..." }`

---

### `archival-search`

Search archival memory using hybrid vector + full-text retrieval.

| | |
|---|---|
| **Permission** | `memory.semantic:r` |
| **Network** | No |
| **fs_write** | No |

**Input:**

| Key | Type | Required | Default |
|-----|------|----------|---------|
| `query` | string | Yes | — |
| `top_k` | u64 | No | `5` |

**Output:**
```json
{ "count": 1, "results": [{ "key": "report-2026", "content": "...", "score": 0.87 }] }
```

---

### `memory-block-read`

Read a named context memory block (kernel-managed).

| | |
|---|---|
| **Permission** | `memory.blocks:r` |
| **Network** | No |
| **fs_write** | No |

**Input:** `{ "label": "persona" }`

Returns a `_kernel_action: "memory_block_read"` envelope; the kernel injects the block's content into the context window.

---

### `memory-block-write`

Write or update a named context memory block.

| | |
|---|---|
| **Permission** | `memory.blocks:w` |
| **Network** | No |
| **fs_write** | No |

**Input:** `{ "label": "persona", "content": "You are a concise analyst..." }`

---

### `memory-block-list`

List all named memory blocks for the current agent.

| | |
|---|---|
| **Permission** | `memory.blocks:r` |
| **Network** | No |
| **fs_write** | No |

No required input. Returns array of `{ label, size_bytes, updated_at }`.

---

### `memory-block-delete`

Delete a named memory block.

| | |
|---|---|
| **Permission** | `memory.blocks:w` |
| **Network** | No |
| **fs_write** | No |

**Input:** `{ "label": "old-persona" }`

---

## Tool Manifests

Every tool — built-in or external — is described by a TOML manifest. The kernel reads manifests from `tools/core/` (distribution-shipped) and `tools/user/` (operator-installed).

**Annotated example (`tools/core/file-reader.toml`):**

```toml
[manifest]
name        = "file-reader"         # Tool name used by LLM and CLI
version     = "1.1.0"               # SemVer
description = "Reads files..."      # Human-readable summary
author      = "agentos-core"        # Author identifier
trust_tier  = "core"                # core | verified | community | blocked

# Required for Verified/Community tier:
# author_pubkey = "<64 hex chars — Ed25519 public key>"
# signature     = "<128 hex chars — Ed25519 signature over canonical JSON payload>"

[capabilities_required]
permissions = ["fs.user_data:r"]    # Resources required; format: "resource:ops"

[capabilities_provided]
outputs = ["content.text"]          # Capabilities this tool produces

[intent_schema]
input  = "FileReadIntent"           # Intent type name (for LLM schema)
output = "FileContent"              # Output type name

[sandbox]
network       = false               # Outbound network allowed?
fs_write      = false               # Filesystem writes allowed?
gpu           = false               # GPU access allowed? (default false)
max_memory_mb = 64                  # Memory cap in MiB
max_cpu_ms    = 5000                # CPU time cap in milliseconds
syscalls      = []                  # Optional syscall allowlist (empty = default base set)

# WASM tools only:
# [executor]
# type      = "wasm"
# wasm_path = "my-tool.wasm"        # Relative to manifest directory
```

**All manifest fields:**

| Field | Section | Required | Description |
|-------|---------|----------|-------------|
| `name` | `manifest` | Yes | Unique tool identifier |
| `version` | `manifest` | Yes | SemVer string |
| `description` | `manifest` | Yes | One-line description |
| `author` | `manifest` | Yes | Author name or organization |
| `trust_tier` | `manifest` | Yes | `core`, `verified`, `community`, or `blocked` |
| `author_pubkey` | `manifest` | Verified/Community | Hex-encoded Ed25519 public key (64 chars) |
| `signature` | `manifest` | Verified/Community | Hex-encoded Ed25519 signature (128 chars) |
| `permissions` | `capabilities_required` | Yes | Permission strings, e.g. `["fs.user_data:r"]` |
| `outputs` | `capabilities_provided` | Yes | Capability output labels |
| `input` | `intent_schema` | Yes | Intent schema name |
| `output` | `intent_schema` | Yes | Output schema name |
| `network` | `sandbox` | Yes | Allow outbound network |
| `fs_write` | `sandbox` | Yes | Allow filesystem writes |
| `gpu` | `sandbox` | No | Allow GPU access (default false) |
| `max_memory_mb` | `sandbox` | Yes | Memory cap in MiB |
| `max_cpu_ms` | `sandbox` | Yes | CPU time cap in milliseconds |
| `syscalls` | `sandbox` | No | Explicit syscall allowlist (empty = default) |
| `type` | `executor` | No | `"inline"` (default) or `"wasm"` |
| `wasm_path` | `executor` | WASM only | Path to `.wasm` file, relative to manifest dir |

---

## Trust Tiers

The `trust_tier` field in `[manifest]` controls how the kernel verifies a tool at load time. The `TrustTier` enum in `agentos-types/src/tool.rs` defines four values, ordered from highest to lowest trust:

```rust
pub enum TrustTier {
    Core,       // highest trust
    Verified,
    Community,
    Blocked,    // hard-rejected
}
```

### Core

- Distribution-trusted — shipped as part of the AgentOS release
- No runtime signature check required
- Loaded from `tools/core/`
- All built-in tools use this tier

### Verified

- Community tool reviewed and co-signed by AgentOS maintainers
- Requires `author_pubkey` and `signature` fields in the manifest
- Ed25519 signature verified over the canonical signing payload on every install
- Suitable for well-tested third-party tools endorsed by the project

### Community

- Author-signed only — the kernel verifies the author's own signature
- Requires `author_pubkey` and `signature` fields in the manifest
- Same Ed25519 signature verification algorithm as Verified
- Operators opt in by running `agentctl tool install`; trust is the operator's responsibility

### Blocked

- Hard-rejected — `ToolBlocked` error returned regardless of signature
- Used for revoked or known-malicious tools
- Tools whose `author_pubkey` appears in the Certificate Revocation List (CRL) are also blocked

**Signing payload:** Signatures cover only security-relevant fields, serialized with alphabetically sorted keys and no extra whitespace:

```json
{"author":"...","capabilities":[...],"max_cpu_ms":N,"max_memory_mb":N,"name":"...","network":B,"version":"..."}
```

Mutable metadata fields (`description`, `checksum`) are excluded, so descriptions can be updated without breaking the existing signature.

**CRL:** The kernel loads a JSON array of revoked author public key hex strings. Any tool whose `author_pubkey` appears in this list is rejected even if the signature is valid.

---

## Tool Signing

Three offline CLI commands handle the complete signing workflow. None of them require a running kernel.

### Generate a keypair

```bash
agentctl tool keygen --output tool-keypair.json
# Keypair written to tool-keypair.json
# Public key: a1b2c3d4...
# Keep tool-keypair.json secret — it contains your signing seed.
```

The output file contains:
```json
{
  "pubkey": "<64 hex chars — Ed25519 public key>",
  "seed":   "<64 hex chars — Ed25519 private seed>",
  "algorithm": "Ed25519",
  "note": "Keep seed secret. Only distribute pubkey."
}
```

> **Security:** The `seed` field is your private key. Never commit it to version control or distribute it. Store it in a hardware key store or secrets manager.

### Sign a manifest

```bash
agentctl tool sign --manifest my-tool.toml --key tool-keypair.json
# Signed manifest written to my-tool.toml
# Signature: 3f8a...

# Write to a separate file:
agentctl tool sign --manifest my-tool.toml --key tool-keypair.json --output my-tool-signed.toml
```

This command:
1. Reads the seed from `tool-keypair.json`
2. Injects `author_pubkey` into the manifest's `[manifest]` section
3. Computes the Ed25519 signature over the canonical payload
4. Injects `signature` into the manifest
5. Immediately self-verifies the signature before writing
6. Writes the signed manifest (overwrites source by default)

### Verify a manifest

```bash
agentctl tool verify my-tool.toml
# OK  my-tool (trust_tier=community)

# Exits with code 1 on failure:
# FAIL  my-tool (trust_tier=community): signature verification failed
```

### End-to-end workflow

```bash
# 1. Generate keypair (one-time setup)
agentctl tool keygen --output my-keypair.json

# 2. Create or edit the manifest (set trust_tier = "community")
# ... edit my-tool.toml ...

# 3. Sign it
agentctl tool sign --manifest my-tool.toml --key my-keypair.json

# 4. Verify (sanity check)
agentctl tool verify my-tool.toml

# 5. Install
agentctl tool install my-tool.toml
```

---

## Installing and Removing Tools

These commands require the kernel to be running.

```bash
# List installed tools
agentctl tool list

# Install from a manifest file (kernel verifies trust tier and signature)
agentctl tool install /path/to/my-tool.toml

# Remove a tool
agentctl tool remove my-tool
```

Example `agentctl tool list` output:
```
NAME                 VERSION      TRUST        DESCRIPTION
----------------------------------------------------------------------
file-reader          1.1.0        core         Reads files from the data dire...
http-client          1.0.0        core         Make outbound HTTP requests...
my-tool              0.2.0        community    Does something useful
```

The kernel validates the manifest's trust tier and signature during `install`. A `Blocked` manifest or invalid signature is rejected before being written to the registry. On success, a `ToolInstalled` audit event is written.

---

## Tool Sandboxing

AgentOS uses three distinct sandboxing strategies depending on how a tool is implemented.

### Native Rust tools (inline)

Built-in tools run in-process as compiled Rust code. Sandboxing is enforced at the code level:

- **Path traversal prevention:** `file-reader` and `file-writer` canonicalize the requested path and verify it starts with `data_dir`. Any traversal (`..`) returns `PermissionDenied`.
- **Size guards:** file reads capped at 10 MiB; memory writes capped at 512 KiB; HTTP responses capped at 10 MiB.
- **Permission enforcement:** `ToolRunner` checks `PermissionSet` before calling `execute()` — defence-in-depth on top of the kernel's router-level check.
- **File locking:** A `FileLockRegistry` coordinates exclusive write access across concurrent agents so readers and writers do not corrupt each other.

### WASM tools

Tools loaded from `.wasm` modules run inside the Wasmtime runtime:

- **Capability isolation:** WASI capabilities are constructed from scratch per invocation. The module receives stdin (payload), stderr (for logging), and the `AGENTOS_OUTPUT_FILE` environment variable. It does not receive any preopened filesystem directories, network sockets, or other environment variables unless the manifest explicitly enables them.
- **Memory limits:** Linear memory growth is capped at 256 MiB via a `ResourceLimiter` on the Wasmtime `Store`. Growth requests beyond this cap are denied.
- **Epoch interruption:** The kernel sets `store.set_epoch_deadline(1)` before calling `_start`. A background Tokio task fires after `max_cpu_ms` milliseconds and calls `engine.increment_epoch()`. Wasmtime interrupts the module and the kernel returns a timeout error.
- **Output file cleanup:** A RAII guard deletes the output file after the result is read, whether execution succeeded or failed. Stale output files from crashed modules do not accumulate.

### Shell-exec (bwrap)

`shell-exec` uses Linux Bubblewrap (`bwrap`) for Linux namespace isolation:

- Root filesystem mounted read-only (`/usr`, `/lib`, `/bin`, `/sbin`)
- Only `data_dir` is writable
- Sensitive directories hidden behind tmpfs: `/etc`, `/var`, `/root`, `/home`
- Fresh `/tmp`, `/dev`, `/proc` provided
- All namespaces unshared (`--unshare-all`)
- Network isolated by default; opt-in via `allow_network: true`

`bwrap` is **required** — `shell-exec` refuses to run and returns an error if bubblewrap is not installed. Running arbitrary shell commands without namespace isolation is never permitted.

---

## Related

- [[17-WASM Tools Development]] — Writing custom WASM and native Rust tools
- [[Architecture Overview]] — Kernel intent flow and capability token system
- [[04-CLI Reference Complete]] — Full CLI command reference
- [[Security Reference]] — Capability tokens, PermissionSet, vault
