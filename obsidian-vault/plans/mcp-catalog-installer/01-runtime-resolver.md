---
title: Phase 1 — Runtime Resolver (node/python detection)
tags:
  - kernel
  - mcp
  - runtime
  - phase-1
date: 2026-04-18
status: planned
effort: 1d
priority: high
---

# Phase 1 — Runtime Resolver

> Locate a working `node`, `python`, or `python3` binary that satisfies a minimum version. Support nvm, volta, asdf, system PATH, and (Phase 7) a portable bundle.

---

## Why this phase

Context: when the user's shell has nvm loaded, their interactive `PATH` contains `~/.nvm/versions/node/v20.x/bin`. But the AgentOS kernel may have been started from a different shell (service manager, different terminal) where nvm wasn't sourced, leaving system `node` (v12 on Pop!_OS) as the one `#!/usr/bin/env node` resolves to.

This produced the 2026-04-18 Gmail MCP incident, where the server crashed with `SyntaxError: Unexpected token '.'` and the failure surfaced as `"MCP server closed connection unexpectedly"`.

The fix: a runtime resolver that explicitly walks known runtime managers, picks the highest-version binary meeting a declared minimum, and returns an absolute path the kernel hands to the stdio transport — bypassing shebang lookup entirely.

---

## Current → Target

**Current:** stdio transport spawns whatever `command` the user passed. If the command is `gmail-mcp` (shebang: `#!/usr/bin/env node`), it inherits `PATH` and may hit a wrong node.

**Target:** a new `crates/agentos-kernel/src/runtime_resolver.rs` module exposing:
```rust
pub struct ResolvedRuntime {
    pub binary: PathBuf,    // absolute path
    pub version: String,    // e.g. "20.20.1"
    pub source: RuntimeSource, // Nvm / Volta / Asdf / System / Bundled
}

pub fn resolve_node(min_version: &str) -> Result<ResolvedRuntime, AgentOSError>;
pub fn resolve_python(min_version: &str) -> Result<ResolvedRuntime, AgentOSError>;
```

The stdio transport and install command use `ResolvedRuntime::binary` directly, avoiding shebangs.

---

## Detailed subtasks

### 1. Create the module skeleton

**File:** `crates/agentos-kernel/src/runtime_resolver.rs` (new)

```rust
use std::path::{Path, PathBuf};
use std::process::Command;
use crate::error::AgentOSError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum RuntimeSource {
    Bundled,   // ~/.agentos/runtimes/<name>-<version>/
    Nvm,       // ~/.nvm/versions/node/*
    Volta,     // ~/.volta/tools/image/node/*
    Asdf,      // ~/.asdf/installs/nodejs/*
    System,    // resolved via PATH
}

#[derive(Debug, Clone)]
pub struct ResolvedRuntime {
    pub binary: PathBuf,
    pub version: String,
    pub source: RuntimeSource,
}

pub fn resolve_node(min_version: &str) -> Result<ResolvedRuntime, AgentOSError> {
    resolve("node", min_version, &node_candidates())
}

pub fn resolve_python(min_version: &str) -> Result<ResolvedRuntime, AgentOSError> {
    resolve("python", min_version, &python_candidates())
}

fn resolve(
    name: &str,
    min_version: &str,
    candidates: &[(RuntimeSource, PathBuf)],
) -> Result<ResolvedRuntime, AgentOSError> {
    let mut best: Option<ResolvedRuntime> = None;
    for (source, path) in candidates {
        if let Some(version) = probe_version(path) {
            if version_at_least(&version, min_version)
                && best.as_ref().map_or(true, |b| version_gt(&version, &b.version))
            {
                best = Some(ResolvedRuntime {
                    binary: path.clone(),
                    version,
                    source: *source,
                });
            }
        }
    }
    best.ok_or_else(|| AgentOSError::RuntimeNotFound {
        name: name.into(),
        min_version: min_version.into(),
    })
}
```

### 2. Runtime manager probes

Helper functions that return `Vec<(RuntimeSource, PathBuf)>`:

```rust
fn node_candidates() -> Vec<(RuntimeSource, PathBuf)> {
    let mut out = Vec::new();

    // Bundled (Phase 7 will populate)
    if let Some(home) = dirs::home_dir() {
        let bundled = home.join(".agentos/runtimes");
        if bundled.is_dir() {
            for entry in std::fs::read_dir(&bundled).into_iter().flatten().flatten() {
                let path = entry.path().join("bin/node");
                if path.is_file() {
                    out.push((RuntimeSource::Bundled, path));
                }
            }
        }

        // nvm
        let nvm = home.join(".nvm/versions/node");
        if nvm.is_dir() {
            for entry in std::fs::read_dir(&nvm).into_iter().flatten().flatten() {
                let path = entry.path().join("bin/node");
                if path.is_file() {
                    out.push((RuntimeSource::Nvm, path));
                }
            }
        }

        // volta
        let volta = home.join(".volta/tools/image/node");
        if volta.is_dir() {
            for entry in std::fs::read_dir(&volta).into_iter().flatten().flatten() {
                let path = entry.path().join("bin/node");
                if path.is_file() {
                    out.push((RuntimeSource::Volta, path));
                }
            }
        }

        // asdf
        let asdf = home.join(".asdf/installs/nodejs");
        if asdf.is_dir() {
            for entry in std::fs::read_dir(&asdf).into_iter().flatten().flatten() {
                let path = entry.path().join("bin/node");
                if path.is_file() {
                    out.push((RuntimeSource::Asdf, path));
                }
            }
        }
    }

    // system
    if let Ok(path) = which_system("node") {
        out.push((RuntimeSource::System, path));
    }

    out
}
```

`python_candidates()` mirrors this for `python3`/`python` under `~/.pyenv/versions/*/bin/python3` and system PATH. pyenv support is optional — system python3 is usually sufficient.

### 3. Version probing

```rust
fn probe_version(binary: &Path) -> Option<String> {
    let out = Command::new(binary).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // node prints "v20.20.1"; python prints "Python 3.11.4"
    let version = text
        .trim()
        .trim_start_matches('v')
        .trim_start_matches("Python ")
        .to_string();
    Some(version)
}

fn version_at_least(got: &str, min: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split('.').filter_map(|p| p.parse().ok()).collect()
    };
    parse(got) >= parse(min)
}

fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split('.').filter_map(|p| p.parse().ok()).collect()
    };
    parse(a) > parse(b)
}
```

### 4. Error variant

Add to `crates/agentos-types/src/error.rs`:

```rust
#[error("Runtime '{name}' >= {min_version} not found on this host")]
RuntimeNotFound { name: String, min_version: String },
```

### 5. Wire into the stdio transport (opt-in)

Do **not** change the existing `StdioTransport::spawn` signature. Instead, the install command (Phase 4) calls the resolver itself and passes the resolved absolute path as `command`. This keeps the transport layer runtime-agnostic.

However, expose a small helper for the install command to use:

```rust
// in crates/agentos-kernel/src/runtime_resolver.rs
pub fn resolve_by_name(runtime: &str, min: &str) -> Result<ResolvedRuntime, AgentOSError> {
    match runtime {
        "node" => resolve_node(min),
        "python" | "python3" => resolve_python(min),
        other => Err(AgentOSError::RuntimeNotFound {
            name: other.into(),
            min_version: min.into(),
        }),
    }
}
```

### 6. Register in kernel crate

**File:** `crates/agentos-kernel/src/lib.rs`

```rust
pub mod runtime_resolver;
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/runtime_resolver.rs` | New module |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod runtime_resolver;` |
| `crates/agentos-types/src/error.rs` | Add `RuntimeNotFound` variant |
| `crates/agentos-kernel/Cargo.toml` | Add `dirs = "5"` if not already present |

No changes to `agentos-mcp` in this phase.

---

## Dependencies

- **Requires:** None.
- **Blocks:** Phase 2 (catalog parser needs to validate runtime names), Phase 4 (install command calls the resolver), Phase 7 (portable runtime installs into `~/.agentos/runtimes/`).

---

## Test plan

Unit tests in `runtime_resolver.rs`:

1. `version_at_least` — `"20.20.1" >= "18"` ✅; `"12.22.9" >= "18"` ❌; `"3.11.4" >= "3.10"` ✅.
2. `version_gt` — `"20.0.0" > "18.20.5"` ✅.
3. `resolve_node` with mocked candidates (use `tempfile` + fake `node --version` shell scripts to inject versions) — returns highest satisfying min.
4. `resolve_node` with all candidates below min — returns `RuntimeNotFound`.
5. `resolve_node` with no candidates at all — returns `RuntimeNotFound`.
6. Resolver logs chosen binary + source + version at `tracing::info!` level (verify via `tracing-test`).

Integration test (optional, only if CI has nvm): detect `~/.nvm/versions/node/*` entries and confirm `Nvm` source wins over `System` when both are present.

---

## Verification

```bash
# Clean build
cargo build -p agentos-kernel

# Run tests
cargo test -p agentos-kernel runtime_resolver

# Lint & fmt
cargo clippy -p agentos-kernel -- -D warnings
cargo fmt --all -- --check

# Manual smoke (on the incident machine)
cargo run -p agentos-cli -- runtime detect node    # (Phase 4 will add this CLI; for now, a unit test suffices)
```

Expected: logs show `"Resolved node /home/ajas/.nvm/versions/node/v20.20.1/bin/node (v20.20.1, Nvm)"`.

---

## Related

- [[MCP Catalog Installer Plan]]
- [[MCP Catalog Installer Data Flow]]
- [[02-catalog-format-and-registry]]
- [[04-install-command]]
- [[07-portable-runtime-installer]]
