---
title: Phase 1 — ScriptParser + ScriptTool
tags:
  - kernel
  - tools
  - scripting
  - phase-1
date: 2026-04-14
status: planned
effort: 1d
priority: high
---

# Phase 1 — ScriptParser + ScriptTool

> Parse annotation headers from script files and wrap them in an `AgentTool` implementation that executes the script via bwrap with `AGENTOS_INPUT` env var.

---

## Why This Phase

This is the load-bearing piece of Script Modules. Everything else (watcher, CLI, manual) is plumbing around this core. A `ScriptTool` is a zero-copy wrapper: it stores only the script path and its parsed metadata, and at execution time spawns the script as a sandboxed subprocess.

---

## Current → Target State

**Current:** No mechanism to wrap arbitrary scripts as `AgentTool`. Users must implement the trait in Rust.

**Target:** `ScriptParser::parse(path)` → `ScriptAnnotations`. `ScriptTool::new(path, annotations)` → `Box<dyn AgentTool>` that runs the script in bwrap.

---

## Files to Create

| File | Purpose |
|---|---|
| `crates/agentos-tools/src/script_tool.rs` | `ScriptAnnotations`, `ScriptParser`, `ScriptTool` |

## Files to Modify

| File | Change |
|---|---|
| `crates/agentos-tools/src/lib.rs` | `pub mod script_tool;` + re-export public types |

---

## Detailed Subtasks

### 1. Define `ScriptAnnotations`

```rust
/// Parsed metadata extracted from a script file's annotation header.
#[derive(Debug, Clone)]
pub struct ScriptAnnotations {
    /// Tool name (kebab-case). Derived from `@agentos tool:` annotation.
    pub name: String,
    /// Human + LLM readable description.
    pub description: String,
    /// Version string (semver).
    pub version: String,
    /// Permission strings e.g. ["fs.data:r", "network.outbound:x"]
    pub permissions: Vec<String>,
    /// Risk class string e.g. "readonly_scoped"
    pub risk: String,
    /// Max execution seconds.
    pub timeout_secs: u64,
    /// Capability tags for discoverability.
    pub tags: Vec<String>,
    /// Whether to pass --share-net to bwrap.
    pub allow_network: bool,
    /// Free-text description of expected input shape (injected into LLM description).
    pub input_hint: Option<String>,
}
```

### 2. Define `ScriptParser`

```rust
pub struct ScriptParser;

impl ScriptParser {
    /// Parse a script file. Returns `None` if the file has no `@agentos tool:` annotation
    /// (it should be silently ignored). Returns `Err` only on I/O failure.
    pub fn parse(path: &Path) -> Result<Option<ScriptAnnotations>, AgentOSError> {
        let content = std::fs::read_to_string(path)?;
        let comment_prefix = Self::detect_comment_prefix(&content, path);
        Self::extract_annotations(&content, comment_prefix)
    }

    fn detect_comment_prefix(content: &str, path: &Path) -> &'static str {
        // Check shebang line first
        if let Some(first_line) = content.lines().next() {
            if first_line.starts_with("#!") {
                let shebang = first_line.to_lowercase();
                if shebang.contains("node") || shebang.contains("deno") || shebang.contains("ts-node") {
                    return "//";
                }
                if shebang.contains("lua") {
                    return "--";
                }
                // bash, sh, python, ruby, perl, php, r → "#"
                return "#";
            }
        }
        // Fall back on file extension
        match path.extension().and_then(|e| e.to_str()) {
            Some("js") | Some("ts") | Some("mjs") => "//",
            Some("lua") => "--",
            _ => "#",
        }
    }

    fn extract_annotations(
        content: &str,
        comment_prefix: &str,
    ) -> Result<Option<ScriptAnnotations>, AgentOSError> {
        let mut name: Option<String> = None;
        let mut description = String::new();
        let mut version = "0.1.0".to_string();
        let mut permissions: Vec<String> = vec!["fs.data:r".to_string()];
        let mut risk = "readonly_scoped".to_string();
        let mut timeout_secs: u64 = 30;
        let mut tags: Vec<String> = Vec::new();
        let mut allow_network = false;
        let mut input_hint: Option<String> = None;

        // Only scan the first 60 lines for annotations
        for line in content.lines().take(60) {
            let trimmed = line.trim();
            // Accept lines that start with the comment prefix
            let rest = if let Some(r) = trimmed.strip_prefix(comment_prefix) {
                r.trim()
            } else {
                continue;
            };
            // Stop at the first non-annotation comment line after annotations start
            if !rest.starts_with('@') {
                continue;
            }
            if let Some(val) = rest.strip_prefix("@agentos tool:").map(str::trim) {
                name = Some(val.to_string());
            } else if let Some(val) = rest.strip_prefix("@description:").map(str::trim) {
                description = val.to_string();
            } else if let Some(val) = rest.strip_prefix("@version:").map(str::trim) {
                version = val.to_string();
            } else if let Some(val) = rest.strip_prefix("@permissions:").map(str::trim) {
                permissions = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            } else if let Some(val) = rest.strip_prefix("@risk:").map(str::trim) {
                risk = val.to_string();
            } else if let Some(val) = rest.strip_prefix("@timeout:").map(str::trim) {
                timeout_secs = val.parse().unwrap_or(30);
            } else if let Some(val) = rest.strip_prefix("@tags:").map(str::trim) {
                tags = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            } else if let Some(val) = rest.strip_prefix("@network:").map(str::trim) {
                allow_network = val == "true";
            } else if let Some(val) = rest.strip_prefix("@input:").map(str::trim) {
                input_hint = Some(val.to_string());
            }
        }

        let Some(name) = name else {
            return Ok(None); // No @agentos tool: annotation — silently skip
        };

        // Validate name: kebab-case only, no path separators
        if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') || name.contains('/') || name.contains('\\') || name.contains('.') {
            return Err(AgentOSError::SchemaValidation(
                format!("Script tool name '{}' must be kebab-case with no path separators", name)
            ));
        }

        // If network permission is in permissions list, set allow_network
        let allow_network = allow_network || permissions.iter().any(|p| p.contains("network.outbound"));

        // Build description including input hint
        let full_description = if let Some(hint) = &input_hint {
            format!("{} Input: {}", description, hint)
        } else {
            description
        };

        Ok(Some(ScriptAnnotations {
            name,
            description: full_description,
            version,
            permissions,
            risk,
            timeout_secs,
            tags,
            allow_network,
            input_hint,
        }))
    }
}
```

### 3. Define `ScriptTool`

```rust
pub struct ScriptTool {
    script_path: PathBuf,
    annotations: ScriptAnnotations,
    parsed_permissions: Vec<(String, PermissionOp)>,
}

impl ScriptTool {
    pub fn new(script_path: PathBuf, annotations: ScriptAnnotations) -> Result<Self, AgentOSError> {
        let parsed_permissions = Self::parse_permissions(&annotations.permissions)?;
        Ok(Self { script_path, annotations, parsed_permissions })
    }

    fn parse_permissions(perms: &[String]) -> Result<Vec<(String, PermissionOp)>, AgentOSError> {
        // Parse "resource:ops" strings into (resource, PermissionOp) pairs
        // "fs.data:r" → ("fs.data", PermissionOp::Read)
        // "fs.data:rw" → ("fs.data", Read), ("fs.data", Write)
        // ... (same logic as SDK macro)
    }
}

#[async_trait]
impl AgentTool for ScriptTool {
    fn name(&self) -> &str { &self.annotations.name }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        self.parsed_permissions.clone()
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let input_json = payload.to_string();
        let data_dir_str = context.data_dir.to_string_lossy().to_string();
        let timeout = Duration::from_secs(self.annotations.timeout_secs);

        // Build bwrap-sandboxed command (mirrors ShellExec logic)
        let bwrap_check = Command::new("bwrap").arg("--version").output().await;
        if bwrap_check.is_err() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: self.annotations.name.clone(),
                reason: "bwrap is required for script tool execution".into(),
            });
        }

        let script_path_str = self.script_path.to_string_lossy().to_string();
        let mut cmd = Command::new("bwrap");
        cmd
            .arg("--ro-bind").arg("/usr").arg("/usr")
            .arg("--ro-bind").arg("/lib").arg("/lib")
            .arg("--ro-bind").arg("/lib64").arg("/lib64")
            .arg("--ro-bind").arg("/bin").arg("/bin")
            .arg("--ro-bind").arg("/sbin").arg("/sbin")
            // Bind the script file read-only
            .arg("--ro-bind").arg(&script_path_str).arg(&script_path_str)
            // Bind data dir writable
            .arg("--bind").arg(&data_dir_str).arg(&data_dir_str)
            .arg("--tmpfs").arg("/root")
            .arg("--tmpfs").arg("/etc")
            .arg("--tmpfs").arg("/var")
            .arg("--tmpfs").arg("/home")
            .arg("--tmpfs").arg("/tmp")
            .arg("--dev").arg("/dev")
            .arg("--proc").arg("/proc")
            .arg("--unshare-all");

        if self.annotations.allow_network {
            cmd.arg("--share-net");
        }

        cmd
            .arg("--chdir").arg(&data_dir_str)
            .arg("--")
            .arg(&script_path_str)
            // Set payload as env var, plus context vars
            .env("AGENTOS_INPUT", &input_json)
            .env("AGENTOS_TASK_ID", context.task_id.to_string())
            .env("AGENTOS_AGENT_ID", context.agent_id.to_string())
            .env("AGENTOS_DATA_DIR", &data_dir_str)
            .kill_on_drop(true);

        let output = tokio::select! {
            result = tokio::time::timeout(timeout, cmd.output()) => {
                result.map_err(|_| AgentOSError::ToolExecutionFailed {
                    tool_name: self.annotations.name.clone(),
                    reason: format!("Script timed out after {}s", self.annotations.timeout_secs),
                })??
            }
            _ = context.cancellation_token.cancelled() => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: self.annotations.name.clone(),
                    reason: "Tool execution cancelled".into(),
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: self.annotations.name.clone(),
                reason: format!("Script exited with non-zero status: {}", stderr.trim()),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str::<serde_json::Value>(stdout.trim()).map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: self.annotations.name.clone(),
                reason: format!("Script stdout is not valid JSON: {}. Output: {}", e, &stdout[..stdout.len().min(200)]),
            }
        })
    }
}
```

---

## Verification

```bash
# Unit test: parse a bash script with annotations
cargo test -p agentos-tools script_parser

# Unit test: parse a Python script
cargo test -p agentos-tools script_parser_python

# Integration test: execute a script tool
cargo test -p agentos-tools script_tool_execute

# Build check
cargo build -p agentos-tools
cargo clippy -p agentos-tools -- -D warnings
```

---

## Dependencies

- Requires `bwrap` at runtime (same as `shell-exec`) — checked at execute time, not parse time
- `tokio::process::Command` — already in scope
- `async_trait` — already in scope
- No new crate dependencies

---

## Test Plan

| Test | Assertion |
|---|---|
| `parse_bash_script` | Extracts name, description, permissions from `#` comment headers |
| `parse_python_script` | Extracts annotations from `#!/usr/bin/env python3` script |
| `parse_js_script` | Extracts annotations from `//` comment headers |
| `parse_no_annotation` | Returns `Ok(None)` — file silently ignored |
| `parse_invalid_name` | Returns `Err(SchemaValidation)` for names with `/` or `.` |
| `parse_network_permission` | Sets `allow_network = true` when `@permissions: network.outbound:x` |
| `execute_returns_json` | Script writing `{"ok": true}` to stdout returns that value |
| `execute_timeout` | Script that sleeps > timeout returns `ToolExecutionFailed` |
| `execute_nonzero_exit` | Script that exits 1 returns `ToolExecutionFailed` with stderr message |
| `execute_invalid_json` | Script printing plain text returns `ToolExecutionFailed` |
