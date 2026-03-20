---
title: "Workspace Directory Mapping for File Tools"
tags:
  - next-steps
  - tools
  - filesystem
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 4h
priority: medium
---

# Workspace Directory Mapping for File Tools

> Add configurable workspace directories beyond `data_dir` so agents can work on real projects, not just the sandboxed data directory.

## What to Do

All file operations are confined to `data_dir` (default: `/tmp/agentos/data`). In real agentic workflows, agents need to read project source code, write test files, and run builds. This makes AgentOS suitable for sandboxed data tasks but not for general software engineering workflows.

### Steps

1. **Add workspace config** to `config/default.toml`:
   ```toml
   [tools.workspace]
   # Additional directories the agent can access (beyond data_dir)
   # Each must be an absolute path. Agent needs explicit permission grant.
   allowed_paths = []
   # Example: allowed_paths = ["/home/user/project", "/shared/data"]
   ```

2. **Update `ToolExecutionContext`** in `crates/agentos-tools/src/traits.rs`:
   - Add `workspace_paths: Vec<PathBuf>` field
   - These are additional directories the agent can access with appropriate permissions

3. **Update file tool path validation** (file-reader, file-editor, file-writer, etc.):
   - Current: `canonical_path.starts_with(data_dir)` → reject
   - New: `canonical_path.starts_with(data_dir) || workspace_paths.any(|wp| canonical_path.starts_with(wp))` → allow
   - Path traversal (`..`) is still blocked on all paths
   - Permission check: require matching `fs.workspace:r` or `fs.workspace:rw` capability

4. **Shell-exec working directory** — add `working_dir` parameter:
   - Must be within `data_dir` or a `workspace_path`
   - Passed to bwrap as bind mount

5. **Add security constraints:**
   - Workspace paths must be absolute
   - Cannot be system directories (`/`, `/etc`, `/var`, `/root`, `/home` without a subdirectory)
   - Validated at config load time

## Files Changed

| File | Change |
|------|--------|
| `config/default.toml` | Add `[tools.workspace]` section |
| `crates/agentos-tools/src/traits.rs` | Add `workspace_paths` to context |
| File tool implementations | Update path validation logic |
| `crates/agentos-tools/src/runner.rs` | Pass workspace_paths from config |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-tools
cargo clippy --workspace -- -D warnings
```

Test: with workspace_path `/tmp/test-project` configured, file-reader can read `/tmp/test-project/src/main.rs`. Without it, the same path is rejected. Path traversal `../../../etc/passwd` from workspace is still blocked.
