---
title: "Phase 6: Managed Builds"
tags:
  - kernel
  - capabilities
  - v4
  - phase-6
date: 2026-04-12
status: planned
effort: 2.5d
priority: high
---

# Phase 6: Managed Builds (`build.*`)

> Enable agents to compile code, run tests, execute linters, and retrieve artifacts — the core software engineering loop — inside managed workspaces with structured output parsing.

---

## Why This Phase

This is the phase that closes the gap with OpenHands/Devin for software engineering tasks. An agent that can install packages (Phase 2) and access project files (Phase 3) but can't run `cargo test` or `pytest` is still incomplete. The build loop — edit, compile, test, fix — is the fundamental cycle of software development.

Current AgentOS can read and write files. With Phases 2+3+6, it can read files, write files, install dependencies, AND verify that the code works. This is the difference between "agent that writes code" and "agent that ships code."

**Key design choice:** Build output is parsed into structured JSON (test results, compiler errors, lint warnings) rather than returned as raw stdout. This saves LLM tokens, reduces hallucination from parsing, and enables programmatic responses.

---

## Current State

- Shell-exec can run arbitrary commands in bwrap sandbox
- No structured output parsing for any build tool
- No concept of build workspace with dependencies available
- Agents can't chain: install deps → run tests → read results → fix code

## Target State

- `BuildProvider` implements `CapabilityProvider` for domain `"build"`
- Actions: `run`, `test`, `lint`, `artifact`
- Builds run inside bwrap with workspace + environment available
- Structured output parsers for cargo, pytest, npm, generic
- Network disabled during builds by default (supply chain safety)
- Resource limits via cgroups or rlimits

---

## Detailed Subtasks

### 1. Define build model

**File:** `crates/agentos-kernel/src/managed_build.rs` (new)

```rust
use serde::{Deserialize, Serialize};

/// Structured build result returned to agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub status: BuildStatus,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub memory_peak_mb: Option<u64>,
    /// Parsed test results (if applicable)
    pub tests: Option<TestSummary>,
    /// Parsed compiler/linter diagnostics
    pub diagnostics: Vec<Diagnostic>,
    /// Raw output (truncated, as fallback)
    pub raw_output: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BuildStatus {
    Success,
    Failed,
    Timeout,
    OOM,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub ignored: u32,
    pub failures: Vec<TestFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    pub name: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DiagnosticLevel {
    Error,
    Warning,
    Info,
    Hint,
}
```

### 2. Output parsers

**File:** `crates/agentos-kernel/src/build_parsers.rs` (new)

Parsers for common build tool output formats:

- **Cargo (Rust):** Parse `cargo test -- --format=json` and `cargo build --message-format=json`
  - Map `test-result` events to `TestFailure` structs
  - Map `compiler-message` events to `Diagnostic` structs

- **Pytest (Python):** Parse `pytest --tb=short -q` output
  - Regex: `(\d+) passed`, `(\d+) failed`
  - Failure blocks: `FAILED tests/test_foo.py::test_bar - AssertionError: ...`

- **npm/Jest (Node.js):** Parse `jest --json` output (JSON mode)
  - `numPassedTests`, `numFailedTests` from JSON
  - `testResults[].assertionResults[]` for failures

- **Generic fallback:** For unrecognized tools, return raw output with exit code

### 3. Implement `BuildProvider`

Actions:
- **`run`** — Execute a build command:
  1. Validate command against build command allowlist
  2. Check `build.run:x` permission
  3. Resolve workspace and environment (from Phase 2 and 3)
  4. Prepare execution environment:
     - bwrap: workspace + env dirs read-write, rest read-only
     - cgroups: memory, CPU, timeout from config
     - Network: disabled by default (supply chain safety)
     - Seccomp: base + build syscall profile
     - Environment variables: PATH includes workspace env's bin dir
  5. Spawn command, capture stdout+stderr
  6. On exit: parse output with appropriate parser
  7. Enforce output size limit (10MB)
  8. Audit: `BuildExecuted`
  9. Return `BuildResult`

- **`test`** — Shortcut for test commands:
  1. Auto-detect ecosystem from workspace (cargo.toml → Rust, package.json → Node)
  2. Run appropriate test command with JSON output flags
  3. Parse results into `TestSummary`

- **`lint`** — Shortcut for lint commands:
  1. Auto-detect ecosystem
  2. Run linter (clippy, eslint, flake8, etc.)
  3. Parse diagnostics

- **`artifact`** — Retrieve build output:
  1. Read a file from the workspace's build output directory
  2. Return contents (with size limit)

### 4. Build configuration

**File:** `config/default.toml` (add section)

```toml
[capabilities.build]
# Allowed build commands (prefix matching).
# "*" = allow all (not recommended).
allowed_commands = [
    "cargo build", "cargo test", "cargo clippy", "cargo fmt",
    "cargo check", "cargo run",
    "python -m pytest", "pytest", "python -m unittest",
    "pip install",  # when combined with workspace env
    "npm run", "npm test", "npm install",
    "npx jest", "npx eslint", "npx prettier",
    "make", "cmake",
    "go build", "go test", "go vet",
]

# Default resource limits for builds
build_memory_max_bytes = 2_147_483_648  # 2 GB
build_cpu_cores = 4
build_timeout_secs = 300  # 5 minutes

# Network during builds (default: disabled for supply chain safety)
build_network_enabled = false

# Maximum build output capture size
build_output_max_bytes = 10_485_760  # 10 MB
```

### 5. Convenience tools and manifests

- `build-run` — `{ "command": "cargo build", "workspace": "myapp" }`
- `build-test` — `{ "workspace": "myapp" }` (auto-detects ecosystem)
- `build-lint` — `{ "workspace": "myapp" }` (auto-detects ecosystem)

Manifests with `risk_class = "exec_capable"`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/managed_build.rs` | NEW — `BuildProvider`, `BuildResult`, execution logic |
| `crates/agentos-kernel/src/build_parsers.rs` | NEW — Output parsers for cargo, pytest, jest |
| `crates/agentos-kernel/src/lib.rs` | Add modules |
| `crates/agentos-kernel/src/kernel.rs` | Register `BuildProvider` at boot |
| `crates/agentos-tools/src/build_tools.rs` | NEW — 3 convenience tools |
| `crates/agentos-tools/src/factory.rs` | Register build tools |
| `config/default.toml` | Add `[capabilities.build]` section |
| `tools/core/build-*.toml` | NEW — 3 manifests |

---

## Dependencies

- **Requires:** Phase 1 (trait), Phase 2 (environments for deps), Phase 3 (storage for project access)
- **Blocks:** Nothing

---

## Test Plan

- [ ] `build.run` executes allowed command and returns structured result
- [ ] `build.run` rejects disallowed commands
- [ ] Cargo test JSON output parsed into `TestSummary` correctly
- [ ] Pytest output parsed into `TestSummary` correctly
- [ ] Compiler errors parsed into `Diagnostic` structs
- [ ] Build timeout enforced — process killed after deadline
- [ ] Build memory limit enforced — OOM returns `BuildStatus::OOM`
- [ ] Network disabled during builds (DNS resolution fails)
- [ ] Output size limit enforced (truncated at max)
- [ ] `build.test` auto-detects Rust workspace and runs `cargo test`
- [ ] `build.test` auto-detects Python project and runs `pytest`
- [ ] Environment from Phase 2 workspace available in PATH during build
- [ ] Storage zone from Phase 3 accessible as working directory
- [ ] Audit events: `BuildExecuted`, `BuildFailed`
- [ ] Generic fallback returns raw output for unrecognized tools

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- managed_build
cargo test -p agentos-kernel -- build_parsers
cargo test -p agentos-tools -- build_tools
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Related

- [[01-capability-provider-trait]] — prerequisite
- [[02-managed-environments]] — workspace environments for builds
- [[03-managed-storage-zones]] — project directory access
- [[Kernel Mediated Capabilities Plan]]
