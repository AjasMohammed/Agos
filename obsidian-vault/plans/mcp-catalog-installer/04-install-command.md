---
title: Phase 4 — One-Command Install Flow
tags:
  - kernel
  - cli
  - mcp
  - install
  - phase-4
date: 2026-04-18
status: planned
effort: 2d
priority: high
---

# Phase 4 — One-Command Install Flow

> `agentos mcp install <id>` resolves the runtime, prefetches the package, orchestrates auth, and attaches — all from a single CLI invocation.

---

## Why this phase

Phases 1–3 deliver the parts. This phase assembles them into the flow that actually removes friction:

1. Look up catalog entry.
2. Enforce trust-tier policy.
3. Resolve runtime (Phase 1).
4. Prefetch the package with a long timeout.
5. Check credentials; hand off to Phase 5 auth helper if missing.
6. Expand env placeholders (`{home}`, `vault:*`).
7. Call the existing `cmd_mcp_attach` with fully-resolved args.
8. Persist an install record so `mcp update` and `mcp uninstall` have context.

The user types one command. The kernel does the rest.

---

## Current → Target

**Current:** `agentos mcp attach` requires the user to pre-install and locate everything.

**Target:** 
```bash
agentos mcp install gmail
# Interactive: confirms trust tier, runs OAuth helper if needed.
# Non-interactive: 
agentos mcp install gmail --yes --unsafe-allow-community
```

---

## Detailed subtasks

### 1. CLI subcommand

**File:** `crates/agentos-cli/src/commands/mcp.rs`

```rust
pub enum McpCommands {
    // …
    /// Install an MCP server from the catalog in one step.
    Install {
        /// Catalog id (e.g. "gmail"). Run `mcp catalog list` to see options.
        id: String,

        /// Skip interactive confirmations.
        #[arg(long)]
        yes: bool,

        /// Allow installing community-tier entries.
        #[arg(long)]
        unsafe_allow_community: bool,

        /// Override the runtime binary path (skips resolver).
        #[arg(long)]
        runtime_binary: Option<PathBuf>,

        /// Skip the OAuth/auth-helper step. Attach will fail if credentials are missing.
        #[arg(long)]
        no_auth: bool,
    },

    /// Remove an installed MCP server.
    Uninstall {
        id: String,
        /// Also remove cached packages and OAuth credentials.
        #[arg(long)]
        purge: bool,
    },

    /// Update an installed MCP server to the latest catalog version.
    Update { id: String },
}
```

### 2. Kernel command + response

**File:** `crates/agentos-bus/src/message.rs`

```rust
KernelCommand::McpInstall {
    id: String,
    assume_yes: bool,
    allow_community: bool,
    runtime_binary_override: Option<PathBuf>,
    no_auth: bool,
},
KernelCommand::McpUninstall { id: String, purge: bool },
KernelCommand::McpCatalogUpdate { id: String },

KernelResponse::McpInstallPrompt {
    kind: InstallPromptKind, // TrustConfirm | AuthMissing | ReadyToAttach
    details: String,
},
KernelResponse::McpInstalled {
    id: String,
    attached_name: String,
    tools_count: usize,
    runtime_used: Option<String>,
},
```

**Prompt-response protocol:** the kernel emits `McpInstallPrompt`; the CLI prints the prompt, reads user input (or applies `--yes`), and sends `KernelCommand::McpInstallContinue { id, approved: bool }` back. State for the in-progress install lives in the kernel keyed by install id.

If prompt/continue round-trips feel too complex, an alternative: run the entire install in one kernel command, accept `assume_yes` + `allow_community` flags up front, and fall back to an error-with-remediation when user input is needed. Prefer this simpler model for v1.

### 3. Install handler (simpler v1)

**File:** `crates/agentos-kernel/src/commands/mcp_install.rs` (new)

```rust
pub async fn cmd_mcp_install(
    &self,
    id: String,
    assume_yes: bool,
    allow_community: bool,
    runtime_binary_override: Option<PathBuf>,
    no_auth: bool,
) -> KernelResponse {
    // 1. Look up.
    let entry = match self.mcp_catalog.lookup(&id) {
        Some(e) => e.clone(),
        None => return KernelResponse::Error {
            message: format!("No catalog entry '{id}'. Try: agentos mcp catalog search <keyword>"),
        },
    };

    // 2. Trust policy.
    if entry.trust_tier == TrustTier::Community && !allow_community {
        return KernelResponse::Error {
            message: format!(
                "'{id}' is community-tier (not vetted). Re-run with --unsafe-allow-community to install."
            ),
        };
    }
    if entry.trust_tier == TrustTier::Blocked {
        return KernelResponse::Error {
            message: format!("'{id}' is blocked by policy."),
        };
    }

    // 3. Verify signature for Verified tier.
    if entry.trust_tier == TrustTier::Verified {
        verify_catalog_signature(&entry)?;
    }

    // 4. Resolve runtime (stdio only).
    let (cmd, args, env) = match (&entry.mcp.transport, &entry.mcp.install) {
        (McpTransportKind::Stdio, InstallBlock::Npx { package, package_version, entry_js, args, prefetch_timeout_secs }) => {
            let runtime = resolve_runtime(&entry, runtime_binary_override.as_deref())?;
            // Prefetch package.
            prefetch_npx(package, package_version, *prefetch_timeout_secs).await?;
            let pkg_root = locate_npx_cache(package, package_version)?;
            let entry_path = pkg_root.join(entry_js);
            let all_args = std::iter::once(entry_path.to_string_lossy().to_string())
                .chain(args.iter().map(|a| a.value.clone()))
                .collect::<Vec<_>>();
            (runtime.binary.to_string_lossy().into_owned(), all_args, expand_env(&entry.mcp.env)?)
        }
        (McpTransportKind::Stdio, InstallBlock::Global { package, package_version, binary, args }) => {
            ensure_global_installed(package, package_version).await?;
            let bin_path = which_binary(binary, &entry.mcp.runtime)?;
            (bin_path.to_string_lossy().into_owned(),
             args.iter().map(|a| a.value.clone()).collect(),
             expand_env(&entry.mcp.env)?)
        }
        (McpTransportKind::Stdio, InstallBlock::Pip { package, package_version, module, args }) => {
            ensure_pip_installed(package, package_version).await?;
            let python = runtime_resolver::resolve_python(
                entry.mcp.runtime_min_version.as_deref().unwrap_or("3.9"),
            )?;
            let mut all_args = vec!["-m".into(), module.clone()];
            all_args.extend(args.iter().map(|a| a.value.clone()));
            (python.binary.to_string_lossy().into_owned(), all_args, expand_env(&entry.mcp.env)?)
        }
        (McpTransportKind::Http, InstallBlock::Prebuilt { url }) => {
            // HTTP: no local install. Go directly to attach via URL.
            return self.cmd_mcp_attach_http_from_catalog(&entry, url).await;
        }
        _ => return KernelResponse::Error {
            message: format!("Unsupported transport/install combination for '{id}'"),
        },
    };

    // 5. Auth.
    if !no_auth {
        if let Some(auth) = &entry.mcp.auth {
            match self.ensure_credentials(&entry, auth, assume_yes).await {
                Ok(()) => (),
                Err(e) => return KernelResponse::Error { message: e.to_string() },
            }
        }
    }

    // 6. Attach via existing code path.
    let response = self.cmd_mcp_attach(
        id.clone(),
        cmd.into(),
        args,
        None,          // url (stdio)
        None,          // static token
        None,          // oauth connector
        Some(entry.mcp.timeout_secs),
        env,
    ).await;

    // 7. Record install (for uninstall/update).
    match response {
        KernelResponse::McpAttached { tools_count, .. } => {
            self.record_install(&entry).await.ok();
            KernelResponse::McpInstalled {
                id,
                attached_name: entry.id.clone(),
                tools_count,
                runtime_used: runtime_binary_override
                    .map(|p| p.to_string_lossy().into_owned())
                    .or_else(|| entry.mcp.runtime.clone()),
            }
        }
        other => other,
    }
}
```

### 4. Package prefetch

Cache under `~/.agentos/mcp-cache/<package>@<version>/`.

```rust
async fn prefetch_npx(package: &str, version: &str, timeout_secs: u64) -> Result<(), AgentOSError> {
    let spec = if version == "*" { package.to_string() } else { format!("{package}@{version}") };
    let cache_dir = agentos_home()?.join("mcp-cache");
    tokio::fs::create_dir_all(&cache_dir).await?;

    // npx with --prefix keeps the cache local.
    let out = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        tokio::process::Command::new("npx")
            .args(["--yes", "--prefix", &cache_dir.to_string_lossy(), &spec, "--version"])
            .output(),
    )
    .await
    .map_err(|_| AgentOSError::McpInstall(format!("Prefetch of {spec} timed out after {timeout_secs}s")))?
    .map_err(|e| AgentOSError::McpInstall(format!("Failed to spawn npx: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(AgentOSError::McpInstall(format!(
            "Package fetch failed: {stderr}"
        )));
    }
    Ok(())
}
```

### 5. Env placeholder expansion

```rust
fn expand_env(env: &HashMap<String, String>) -> Result<HashMap<String, String>, AgentOSError> {
    let mut out = HashMap::new();
    let home = dirs::home_dir()
        .ok_or_else(|| AgentOSError::McpInstall("Cannot locate $HOME".into()))?;
    for (k, v) in env {
        let expanded = v.replace("{home}", &home.to_string_lossy());
        out.insert(k.clone(), expanded);
        // `vault:*` refs are resolved later by the attach path, so leave intact.
    }
    Ok(out)
}
```

### 6. Install record

SQLite table adjacent to `mcp_attachments.db`:

```sql
CREATE TABLE IF NOT EXISTS mcp_install_records (
    id TEXT PRIMARY KEY,
    catalog_version TEXT NOT NULL,
    trust_tier TEXT NOT NULL,
    installed_at INTEGER NOT NULL,
    runtime_binary TEXT,
    package TEXT,
    package_version TEXT
);
```

Purpose: `mcp update <id>` compares `catalog_version` vs current catalog entry; `mcp uninstall --purge` reads `package` + `credentials_path` to clean up.

### 7. Uninstall & update

- **Uninstall:** call `cmd_mcp_detach(id)`, delete install record, optionally remove cached package dir and credentials file.
- **Update:** diff catalog version vs install record, prompt user with the changes, on approval run detach + install.

---

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/commands/mcp_install.rs` | New module |
| `crates/agentos-kernel/src/commands/mod.rs` | Add `pub mod mcp_install;` |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch install/uninstall/update |
| `crates/agentos-kernel/src/install_store.rs` | New SQLite store |
| `crates/agentos-bus/src/message.rs` | Install/uninstall/update command + response variants |
| `crates/agentos-cli/src/commands/mcp.rs` | Install/uninstall/update subcommands + handlers |
| `crates/agentos-cli/src/main.rs` | Route subcommands |
| `crates/agentos-types/src/error.rs` | Add `McpInstall(String)` variant |

Target: 7–8 files changed. If this grows beyond 10 on implementation, split the install-record SQLite store into a Phase 4b sub-task.

---

## Dependencies

- **Requires:** Phase 1 (runtime resolver), Phase 2 (catalog registry).
- **Blocks:** Phase 5 (auth helper plugs into `ensure_credentials`). Phase 6 relies on install working end-to-end.

---

## Test plan

1. Install unknown id → error mentions `catalog search`.
2. Install community-tier without `--unsafe-allow-community` → rejected with remediation message.
3. Install blocked-tier → rejected with "blocked by policy".
4. Install HTTP/prebuilt entry → skips runtime resolver, calls HTTP attach path.
5. Install npx entry → prefetch hits cache dir; attach succeeds; install record created.
6. Re-install same id → detaches old, re-prefetches, re-attaches; single install record.
7. Uninstall removes attachment + install record; `--purge` also deletes cache + credentials file.
8. Update with identical catalog version → "already up to date" message, no-op.
9. Env var `{home}` correctly expanded; `vault:` refs preserved for downstream resolver.

Integration fixture: minimal catalog entry pointing at a stub Node MCP server in `target/test-stubs/` to avoid real network.

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel mcp_install
cargo clippy -p agentos-kernel -- -D warnings
cargo fmt --all -- --check

# Manual smoke (after Phase 6 seeds gmail):
agentos kernel start
agentos mcp install gmail --yes
agentos mcp status
agentos mcp tools
agentos mcp uninstall gmail --purge
```

---

## Related

- [[MCP Catalog Installer Plan]]
- [[MCP Catalog Installer Data Flow]]
- [[01-runtime-resolver]]
- [[02-catalog-format-and-registry]]
- [[05-oauth-helper-automation]]
- [[06-seed-catalog-entries]]
