---
title: Phase 5 — OAuth Helper Automation
tags:
  - kernel
  - mcp
  - oauth
  - auth
  - phase-5
date: 2026-04-18
status: planned
effort: 1d
priority: high
---

# Phase 5 — OAuth Helper Automation

> Detect missing credentials for a catalog entry, run the auth helper subprocess, and wait for the credentials file to appear — all orchestrated by the install command.

---

## Why this phase

The current Gmail flow forces the user to:

```bash
npx @gongrzhe/server-gmail-autoauth-mcp auth   # run separately, in another terminal
# … browser pops up, user approves, credentials.json is written
```

then come back and attach. That's the last remaining manual step.

If a catalog entry declares `[mcp.auth]` with `type = "oauth"`, the kernel should:

1. Check whether `credentials_path` exists.
2. If not, prompt the user: "Run auth helper now? [Y/n]".
3. On confirmation, spawn the helper subprocess **in the foreground**, printing its output.
4. Wait for the subprocess to exit.
5. Verify the credentials file now exists.
6. Only then proceed to attach.

We intentionally do not manage refresh tokens or handle the OAuth protocol ourselves. The helper is a black box — it writes a file; we check for the file. That's the full contract.

---

## Current → Target

**Current:** auth helper is documented in the README. No kernel support.

**Target:** `ensure_credentials(&entry, auth, assume_yes)` helper in `mcp_install.rs` that handles the full flow.

---

## Detailed subtasks

### 1. The helper function

**File:** `crates/agentos-kernel/src/commands/mcp_install.rs` (same module as Phase 4)

```rust
async fn ensure_credentials(
    &self,
    entry: &CatalogEntry,
    auth: &AuthBlock,
    assume_yes: bool,
) -> Result<(), AgentOSError> {
    match auth.kind {
        AuthKind::None => Ok(()),
        AuthKind::Oauth => self.ensure_oauth_credentials(entry, auth, assume_yes).await,
        AuthKind::ApiKey => self.ensure_api_key(entry, auth, assume_yes).await,
        AuthKind::AppPassword => self.ensure_api_key(entry, auth, assume_yes).await, // same UX
    }
}

async fn ensure_oauth_credentials(
    &self,
    entry: &CatalogEntry,
    auth: &AuthBlock,
    assume_yes: bool,
) -> Result<(), AgentOSError> {
    let path_template = auth.credentials_path.as_ref()
        .ok_or_else(|| AgentOSError::McpInstall(
            "OAuth auth block is missing credentials_path".into(),
        ))?;
    let path = expand_home(path_template)?;

    if path.exists() {
        tracing::info!(id = %entry.id, path = %path.display(), "oauth credentials present");
        return Ok(());
    }

    if assume_yes {
        // Auto-run the helper without prompting.
    } else {
        // The CLI is responsible for prompting the user. In the single-command
        // model from Phase 4, we bail with a clear remediation so the CLI can
        // re-prompt. Alternatively we return a KernelResponse::McpInstallPrompt
        // variant that the CLI handles. Keep the v1 path simple and return an
        // error; the CLI retries with `--yes` after user confirmation.
        return Err(AgentOSError::McpInstall(format!(
            "OAuth credentials missing at {}. \
             Re-run with --yes to execute the helper: {} {}",
            path.display(),
            auth.helper_command.as_deref().unwrap_or("<unset>"),
            auth.helper_args.join(" "),
        )));
    }

    let helper_cmd = auth.helper_command.as_ref()
        .ok_or_else(|| AgentOSError::McpInstall("OAuth helper_command not configured".into()))?;

    tracing::info!(
        id = %entry.id,
        command = %helper_cmd,
        args = ?auth.helper_args,
        "running oauth helper subprocess"
    );

    // Emit a bus event so the CLI can tell the user what's happening.
    self.audit.log(AuditEvent::McpAuthHelperStarted {
        id: entry.id.clone(),
        command: helper_cmd.clone(),
    }).await;

    // Spawn the helper. Inherit stdio so the user sees the URL / browser output.
    let status = tokio::process::Command::new(helper_cmd)
        .args(&auth.helper_args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .map_err(|e| AgentOSError::McpInstall(format!("Failed to spawn auth helper: {e}")))?;

    if !status.success() {
        self.audit.log(AuditEvent::McpAuthAborted { id: entry.id.clone() }).await;
        return Err(AgentOSError::McpInstall(format!(
            "OAuth helper exited with {status}. Credentials not written."
        )));
    }

    // Verify the file now exists.
    if !path.exists() {
        self.audit.log(AuditEvent::McpAuthAborted { id: entry.id.clone() }).await;
        return Err(AgentOSError::McpInstall(format!(
            "OAuth helper exited successfully but credentials file {} is still missing. \
             Check the helper's output for clues.",
            path.display(),
        )));
    }

    self.audit.log(AuditEvent::McpAuthHelperSucceeded {
        id: entry.id.clone(),
        path: path.to_string_lossy().into_owned(),
    }).await;

    Ok(())
}

async fn ensure_api_key(
    &self,
    entry: &CatalogEntry,
    auth: &AuthBlock,
    _assume_yes: bool,
) -> Result<(), AgentOSError> {
    let env_var = auth.env_var.as_ref()
        .ok_or_else(|| AgentOSError::McpInstall(
            "API key auth block is missing env_var".into(),
        ))?;

    // Check vault first. If missing, bail with a clear next step — the CLI
    // prompts the user to run `agentos secret set <env_var>`.
    if self.vault.get(env_var).await.is_err() {
        return Err(AgentOSError::McpInstall(format!(
            "API key not found in vault. Run: agentos secret set {env_var}"
        )));
    }
    Ok(())
}
```

### 2. Audit events

Add to `crates/agentos-audit/src/event.rs`:

```rust
pub enum AuditEventType {
    // …
    McpAuthHelperStarted,
    McpAuthHelperSucceeded,
    McpAuthAborted,
}
```

### 3. Path expansion helper

```rust
fn expand_home(template: &str) -> Result<PathBuf, AgentOSError> {
    let home = dirs::home_dir().ok_or_else(|| AgentOSError::McpInstall("no $HOME".into()))?;
    Ok(PathBuf::from(template.replace("{home}", &home.to_string_lossy())))
}
```

### 4. CLI interaction model

Keep the v1 model simple: CLI calls `mcp install` with no `--yes` → kernel may return the "credentials missing" error with the helper command. CLI captures this specific error and prompts:

```rust
// crates/agentos-cli/src/commands/mcp.rs
pub async fn cmd_install(bus: &mut BusClient, opts: InstallOpts) -> anyhow::Result<()> {
    let first = bus.send_command(mk_install_cmd(&opts, /*assume_yes=*/false)).await?;
    match first {
        KernelResponse::Error { message } if message.contains("OAuth credentials missing") => {
            eprintln!("{}", message);
            if opts.yes || dialoguer::Confirm::new()
                .with_prompt("Run the helper now?")
                .default(true)
                .interact()?
            {
                let second = bus.send_command(mk_install_cmd(&opts, /*assume_yes=*/true)).await?;
                return handle_final(second);
            }
            anyhow::bail!("install aborted");
        }
        other => handle_final(other),
    }
}
```

This two-round interaction is fine: kernel stays stateless on prompts, CLI owns the user interaction.

### 5. What happens on Ctrl+C mid-helper

- The helper subprocess inherits the CLI's terminal session; Ctrl+C hits the helper.
- Kernel detects non-zero exit status.
- Audit event `McpAuthAborted` is logged.
- Credentials file is (probably) absent → error bubbles up with a clear message.

No partial state is left behind — we never touch the credentials file, only the helper does.

---

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/commands/mcp_install.rs` | Add `ensure_credentials` + OAuth/api-key helpers |
| `crates/agentos-audit/src/event.rs` | Add 3 event types |
| `crates/agentos-audit/src/log.rs` | Map event types to log rows |
| `crates/agentos-cli/src/commands/mcp.rs` | Two-round install flow for missing creds |

---

## Dependencies

- **Requires:** Phase 4 (install command skeleton). 
- **Blocks:** Phase 6 (gmail entry declares OAuth; testing needs this flow). 

---

## Test plan

1. Entry with `auth.type = "none"` → `ensure_credentials` is a no-op.
2. OAuth entry with credentials file present → no-op.
3. OAuth entry without credentials, `assume_yes = false` → error mentions helper command + remediation.
4. OAuth entry without credentials, `assume_yes = true`, helper writes file → success.
5. OAuth entry, helper exits non-zero → error; audit event `McpAuthAborted` recorded.
6. OAuth entry, helper exits zero but file still missing → specific error message.
7. API-key entry with vault secret missing → error mentions `agentos secret set <env_var>`.
8. Ctrl+C during helper → SIGINT propagates, helper exits, kernel reports abort.

Use a **stub helper script** in tests that writes to a tempfile path passed via an env var:

```bash
#!/bin/sh
echo "{ \"access_token\": \"fake\" }" > "$CREDS_OUT"
```

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel mcp_install::auth
cargo clippy -p agentos-kernel -- -D warnings

# Manual smoke (end-to-end with real Gmail helper):
agentos kernel start
agentos mcp install gmail    # first run: prompt for OAuth helper
# (browser pops up, user approves)
# install completes; tools available in `agentos mcp tools`
```

---

## Related

- [[MCP Catalog Installer Plan]]
- [[04-install-command]]
- [[06-seed-catalog-entries]]
