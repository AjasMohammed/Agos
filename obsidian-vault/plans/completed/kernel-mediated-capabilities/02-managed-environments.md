---
title: "Phase 2: Managed Environments"
tags:
  - kernel
  - capabilities
  - v4
  - phase-2
date: 2026-04-12
status: planned
effort: 3d
priority: high
---

# Phase 2: Managed Environments (`env.*`)

> Allow agents to create isolated workspaces, install packages, and manage dependencies — all mediated by the kernel with allowlist policy and per-agent isolation.

---

## Why This Phase

The #1 reason agents can't do real software engineering in AgentOS is: they cannot install dependencies. When an agent encounters `ImportError: No module named 'flask'`, it has no recourse — it can't run `pip install flask` because shell-exec is sandboxed and there's no package management abstraction.

OpenHands solves this by giving agents raw shell access in Docker. We solve it by bringing package management inside the kernel as a mediated capability. The agent says "install flask", the kernel validates against policy, installs into a scoped workspace, and returns a structured result.

**Research backing:** Every competing agent system (OpenHands, Devin, Claude Code) treats environment setup as a prerequisite for autonomy. The WASI model shows that scoped resource grants (specific directories, specific capabilities) are more secure than broad access.

---

## Current State

- `ToolExecutionContext.data_dir` exists — agents have a per-agent data directory (`traits.rs:40`)
- `ToolExecutionContext.workspace_paths` exists — additional allowed directories (`traits.rs:59`)
- `shell_exec.rs` uses bwrap with `data_dir` as the only writable mount
- No package management tools exist
- No workspace isolation beyond `data_dir`

## Target State

- `EnvProvider` implements `CapabilityProvider` for domain `"env"`
- Actions: `create`, `install`, `list`, `destroy`, `snapshot`
- Workspaces scoped per-agent at `{data_dir}/workspaces/{workspace_name}/`
- Package installation into workspace-local paths (venv, node_modules, etc.)
- Package allowlists configurable in `config/default.toml`
- Default allowlists shipped for Python, Node.js, and Rust ecosystems

---

## Detailed Subtasks

### 1. Define workspace model

**File:** `crates/agentos-kernel/src/managed_env.rs` (new)

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// An isolated workspace for an agent's project environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedWorkspace {
    pub name: String,
    pub agent_id: AgentID,
    pub root_path: PathBuf,
    pub ecosystem: Ecosystem,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub packages_installed: Vec<InstalledPackage>,
    pub disk_usage_bytes: u64,
    pub quota_bytes: u64,
}

/// Supported package ecosystems.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Ecosystem {
    Python,
    NodeJs,
    Rust,
    System,  // apt/dnf — requires elevated policy
    Generic, // No package manager, just a directory
}

/// Record of an installed package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub ecosystem: Ecosystem,
    pub installed_at: chrono::DateTime<chrono::Utc>,
    pub size_bytes: u64,
}
```

### 2. Implement `EnvProvider`

**File:** `crates/agentos-kernel/src/managed_env.rs`

Actions:
- **`create`** — Creates workspace directory + ecosystem-specific setup:
  - Python: `python -m venv {workspace}/venv`
  - Node.js: `mkdir {workspace}/node_modules`
  - Rust: `mkdir {workspace}/target`
  - Generic: just create directory
  - Runs via `tokio::task::spawn_blocking` with bwrap (no network)
  
- **`install`** — Installs a package:
  - Validates package name against allowlist (no arbitrary strings)
  - Validates version constraint if provided
  - Runs ecosystem-specific installer:
    - Python: `{workspace}/venv/bin/pip install {pkg}=={version} --no-cache-dir`
    - Node.js: `npm install --prefix {workspace} {pkg}@{version}`
    - Rust: `cargo install {pkg} --root {workspace}`
  - Runs inside bwrap with network enabled for install duration only
  - Captures installed version, size, dependency count
  - Updates `ManagedWorkspace.packages_installed`
  - Checks quota after install; rolls back if exceeded

- **`list`** — Returns list of installed packages in workspace

- **`destroy`** — Removes workspace directory
  - Checks no running processes are using it
  - Audits removal

- **`snapshot`** — Creates a tarball of the workspace for checkpointing

### 3. Package allowlist system

**File:** `config/default.toml` (add section)

```toml
[capabilities.env]
# Maximum workspace size per agent
default_quota_bytes = 2_147_483_648  # 2 GB

# Package allowlists per ecosystem
# "curated" = ship default list, "open" = any package, "locked" = none without approval
python_policy = "curated"
nodejs_policy = "curated"
rust_policy = "curated"
system_policy = "locked"  # apt/dnf always requires approval

# Network timeout for package installation (seconds)
install_timeout_secs = 120
```

**File:** `config/allowlists/python.toml` (new)

```toml
# Curated Python package allowlist
# Packages not on this list require operator approval via escalation
packages = [
    "flask", "django", "fastapi", "uvicorn", "gunicorn",
    "requests", "httpx", "aiohttp",
    "numpy", "pandas", "scipy", "scikit-learn", "matplotlib",
    "pytest", "pytest-cov", "pytest-asyncio",
    "pydantic", "sqlalchemy", "alembic",
    "click", "typer", "rich",
    "python-dotenv", "toml", "pyyaml",
    "pillow", "beautifulsoup4", "lxml",
    "celery", "redis", "boto3",
]
```

Ship similar allowlists for Node.js and Rust.

### 4. Register convenience tools

**File:** `crates/agentos-tools/src/env_tools.rs` (new)

Thin wrapper tools:
- `env-create` — calls `EnvProvider.execute("create", ...)`
- `env-install` — calls `EnvProvider.execute("install", ...)`  
- `env-list` — calls `EnvProvider.execute("list", ...)`
- `env-destroy` — calls `EnvProvider.execute("destroy", ...)`

Each has its own JSON schema so LLMs can call them naturally:
```json
// env-install schema
{
  "package": "flask",
  "version": ">=3.0",
  "workspace": "my-project"
}
```

### 5. Tool manifests

**Directory:** `tools/core/`

- `env-create.toml` — `risk_class = "write_scoped"`, permissions: `env.create:x`
- `env-install.toml` — `risk_class = "write_scoped"`, permissions: `env.install:x`, `net.outbound:x`
- `env-list.toml` — `risk_class = "readonly_scoped"`, permissions: `env.list:r`
- `env-destroy.toml` — `risk_class = "write_scoped"`, permissions: `env.destroy:x`

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/managed_env.rs` | NEW — `ManagedWorkspace`, `EnvProvider`, `Ecosystem` |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod managed_env;` |
| `crates/agentos-kernel/src/kernel.rs` | Register `EnvProvider` in capability registry at boot |
| `crates/agentos-tools/src/env_tools.rs` | NEW — 4 convenience tools |
| `crates/agentos-tools/src/factory.rs` | Register env tools |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod env_tools;` |
| `config/default.toml` | Add `[capabilities.env]` section |
| `config/allowlists/python.toml` | NEW — Python package allowlist |
| `config/allowlists/nodejs.toml` | NEW — Node.js package allowlist |
| `config/allowlists/rust.toml` | NEW — Rust package allowlist |
| `tools/core/env-create.toml` | NEW — Manifest |
| `tools/core/env-install.toml` | NEW — Manifest |
| `tools/core/env-list.toml` | NEW — Manifest |
| `tools/core/env-destroy.toml` | NEW — Manifest |

---

## Dependencies

- **Requires:** Phase 1 (capability provider trait)
- **Blocks:** Phase 6 (managed builds need workspace environments)

---

## Test Plan

- [ ] `env.create` creates workspace directory with correct structure
- [ ] `env.create` for Python creates venv with `bin/pip` present
- [ ] `env.install` for allowed package succeeds and records in workspace
- [ ] `env.install` for disallowed package returns error with escalation suggestion
- [ ] `env.install` respects quota — rolls back if workspace exceeds limit
- [ ] `env.install` runs with network enabled, other actions without
- [ ] `env.list` returns correct installed packages
- [ ] `env.destroy` removes workspace directory completely
- [ ] `env.destroy` fails if processes are running in workspace
- [ ] Workspaces are scoped per-agent — agent B cannot access agent A's workspace
- [ ] Audit events: `EnvironmentCreated`, `PackageInstalled`, `EnvironmentDestroyed`
- [ ] All package installs run via bwrap (no raw shell execution)

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- managed_env
cargo test -p agentos-tools -- env_tools
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Related

- [[01-capability-provider-trait]] — prerequisite
- [[06-managed-builds]] — builds run inside managed workspaces
- [[Kernel Mediated Capabilities Plan]]
