---
title: Phase 7 — Portable Runtime Installer (stretch)
tags:
  - kernel
  - cli
  - runtime
  - phase-7
  - stretch
date: 2026-04-18
status: planned
effort: 1-2d
priority: low
---

# Phase 7 — Portable Runtime Installer (stretch)

> Let users install a known-good node/python runtime into `~/.agentos/runtimes/` so MCP installs work without touching system package managers or nvm/volta/asdf.

---

## Why this phase

The runtime resolver (Phase 1) walks nvm/volta/asdf/system. For users with none of those installed, or running on a system with only an ancient Python, we want a one-liner that drops a compatible runtime into `~/.agentos/runtimes/`.

This is a convenience — the critical path for the feature does not require it. Ship Phases 1–6 first, then add this.

---

## Current → Target

**Current:** user must install node/python via their OS, nvm, volta, or asdf.

**Target:**
```bash
agentos runtime install node@20     # downloads Node.js v20 LTS to ~/.agentos/runtimes/node-20.x.y/
agentos runtime install python@3.11 # downloads python-build-standalone to ~/.agentos/runtimes/python-3.11.x/
agentos runtime list
agentos runtime remove node@20
```

Phase 1's resolver already prefers `RuntimeSource::Bundled` over nvm/volta/asdf/system, so installed runtimes are picked up immediately.

---

## Detailed subtasks

### 1. Top-level `runtime` subcommand

**File:** `crates/agentos-cli/src/commands/runtime.rs` (new)

```rust
#[derive(Subcommand)]
pub enum RuntimeCommands {
    /// Install a portable runtime into ~/.agentos/runtimes/.
    Install {
        /// "node@20", "python@3.11", etc.
        spec: String,
        /// Overwrite if a version with the same major is already installed.
        #[arg(long)]
        force: bool,
    },
    /// List installed runtimes.
    List,
    /// Remove a runtime.
    Remove { spec: String },
    /// Show which runtime the resolver would pick for a given min-version.
    Detect {
        /// "node" or "python".
        name: String,
        /// Minimum version (e.g. "18", "3.11").
        #[arg(long, default_value = "0")]
        min: String,
    },
}
```

Route from `main.rs`:

```rust
Commands::Runtime(sub) => match sub {
    RuntimeCommands::Install { spec, force } => commands::runtime::install(spec, force).await?,
    RuntimeCommands::List => commands::runtime::list().await?,
    RuntimeCommands::Remove { spec } => commands::runtime::remove(spec).await?,
    RuntimeCommands::Detect { name, min } => commands::runtime::detect(&name, &min).await?,
},
```

### 2. Download sources

Avoid rolling our own binary hosting. Pull from well-known release channels:

- **Node.js:** `https://nodejs.org/dist/v{version}/node-v{version}-linux-x64.tar.xz` (and darwin/arm64 variants). Verify SHA256 against `SHASUMS256.txt`.
- **Python:** [python-build-standalone](https://github.com/indygreg/python-build-standalone) releases (`cpython-3.11.x+date-x86_64-unknown-linux-gnu-install_only.tar.gz`). Verify SHA256 from the release asset list.

Release URLs and SHA256 catalog embedded in the binary (updated on each AgentOS release cut).

### 3. Install flow

```rust
pub async fn install(spec: String, force: bool) -> anyhow::Result<()> {
    let parsed = parse_spec(&spec)?; // ("node", "20") or ("python", "3.11")
    let manifest = embedded_release_manifest(&parsed.name)?;
    let release = manifest.pick_latest_for_major(&parsed.version)?;
    let target_dir = agentos_home()?
        .join("runtimes")
        .join(format!("{}-{}", parsed.name, release.version));

    if target_dir.exists() && !force {
        anyhow::bail!("Already installed at {}. Use --force to overwrite.", target_dir.display());
    }

    let tarball = download_and_verify(&release.url, &release.sha256).await?;
    extract_tarball(&tarball, &target_dir).await?;
    verify_binary_works(&target_dir, &parsed.name).await?;

    println!("Installed {} to {}", release.version, target_dir.display());
    Ok(())
}
```

Download uses `reqwest` (already a workspace dependency). Extraction uses `tar` + `xz2` for Node, `tar` + `flate2` for python-build-standalone.

### 4. List / remove / detect

Straightforward reads of `~/.agentos/runtimes/`:

```rust
pub async fn list() -> anyhow::Result<()> {
    let dir = agentos_home()?.join("runtimes");
    if !dir.exists() {
        println!("No runtimes installed.");
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.is_dir() {
            let name = path.file_name().unwrap().to_string_lossy();
            let binary_ok = path.join("bin/node").exists() || path.join("bin/python3").exists();
            println!("  {} {}", name, if binary_ok { "✓" } else { "✗ (corrupt)" });
        }
    }
    Ok(())
}
```

`detect` calls `runtime_resolver::resolve_by_name` and prints the chosen path / version / source.

### 5. Windows support (best-effort)

Use `.zip` variants for Node on Windows; python-build-standalone ships Windows builds too. This phase targets Linux + macOS as v1; file a follow-up for Windows if demand arises.

---

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-cli/src/commands/runtime.rs` | New module |
| `crates/agentos-cli/src/commands/mod.rs` | Add `pub mod runtime;` |
| `crates/agentos-cli/src/main.rs` | Route `Commands::Runtime` |
| `crates/agentos-cli/src/runtime_manifest.rs` | Embedded release manifest (URLs + SHA256s) |
| `crates/agentos-cli/Cargo.toml` | Add `tar`, `xz2`, `flate2` if not already present |
| `docs/guide/06-runtimes.md` | New user guide section |

---

## Dependencies

- **Requires:** Phase 1 (resolver prefers `Bundled`).
- **Blocks:** Nothing critical — stretch goal.

---

## Test plan

1. `install node@20` on clean `~/.agentos/` → downloads, extracts, binary runs.
2. `install node@20` twice → second fails without `--force`, succeeds with.
3. `install node@99` (nonexistent) → clear error listing supported majors.
4. SHA256 mismatch (inject bad bytes) → fails before extract; no partial state.
5. `remove node@20` after install → directory gone.
6. `detect node --min 18` with a bundled v20 + nvm v12 → picks Bundled v20.
7. Install corrupt archive → error; partial extract dir is cleaned up.

Use a local HTTP fixture server to serve test archives so CI doesn't hit nodejs.org.

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-cli runtime
cargo clippy -p agentos-cli -- -D warnings

# Manual smoke:
agentos runtime install node@20
agentos runtime list
agentos runtime detect node --min 18
agentos mcp install gmail   # now uses the bundled node regardless of nvm/system
agentos runtime remove node@20
```

---

## Related

- [[MCP Catalog Installer Plan]]
- [[01-runtime-resolver]]
- [[04-install-command]]
