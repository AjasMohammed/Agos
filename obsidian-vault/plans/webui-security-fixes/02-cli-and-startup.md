---
title: "Phase 02 -- CLI Startup, Graceful Shutdown, and Static Files"
tags:
  - webui
  - cli
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 3h
priority: high
---

# Phase 02 -- CLI Startup, Graceful Shutdown, and Static Files

> Fix vault passphrase leaking via CLI args, wire proper graceful shutdown so kernel and server clean up together, and make static file serving work regardless of CWD.

---

## Why This Phase

Three correctness bugs in the CLI/server startup path: (I4) the `--vault-passphrase` CLI arg is visible in `/proc/PID/cmdline` and the original `String` is not zeroized after wrapping in `ZeroizingString`; (I5) `tokio::select!` drops the losing future without cleanup, orphaning the kernel or server; (I6) static files only load when CWD is the workspace root because `ServeDir` uses a relative path. These are independent of the security middleware work and should be addressed early since they affect the server lifecycle.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Vault passphrase (I4) | `--vault_passphrase` CLI arg on `web.rs:22` stores passphrase in `Option<String>`, visible in `/proc/PID/cmdline`; `handle_serve` wraps in `ZeroizingString` on line 33 but the original `String` parameter remains live until the function returns | Remove `--vault_passphrase` CLI arg entirely; accept passphrase only via `AGENTOS_VAULT_PASSPHRASE` env var or interactive prompt via `rpassword`; wrap in `ZeroizingString` immediately at point of acquisition |
| Graceful shutdown (I5) | `tokio::select!` on `web.rs:56-59` races `kernel.run()` vs `server.start()`; when one completes, the other future is dropped without shutdown hooks | Use `CancellationToken` shared between kernel and server; `Ctrl+C` or either task exiting cancels the token, triggering graceful shutdown of both |
| Static file path (I6) | `ServeDir::new("crates/agentos-web/static")` on `router.rs:31` uses a CWD-relative path; breaks when binary runs from any other directory | Resolve path at compile time via `env!("CARGO_MANIFEST_DIR")` which Cargo sets to the crate's directory |

---

## Subtasks

### 1. Remove `--vault_passphrase` CLI argument and zeroize at boundary

**File:** `crates/agentos-cli/src/commands/web.rs`

The `--vault_passphrase` argument puts the passphrase in `/proc/PID/cmdline`, visible to any user on the system via `ps aux` or `cat /proc/PID/cmdline`.

Change the `WebCommands::Serve` variant (lines 11-24) to remove the argument:

```rust
#[derive(Subcommand, Debug)]
pub enum WebCommands {
    /// Start the web UI server
    Serve {
        /// Port to bind the web server on
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Host/IP to bind on
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        // vault_passphrase argument REMOVED -- use env var or interactive prompt only
    },
}
```

Update `handle_serve` signature (lines 27-32) to remove the `vault_passphrase` parameter:

```rust
pub async fn handle_serve(
    config_path: &Path,
    host: &str,
    port: u16,
) -> anyhow::Result<()> {
    let passphrase = ZeroizingString::new(
        match std::env::var("AGENTOS_VAULT_PASSPHRASE") {
            Ok(env_pass) if !env_pass.is_empty() => env_pass,
            _ => {
                eprint!("Enter vault passphrase: ");
                rpassword::read_password()?
            }
        },
    );
    // ... rest of function
}
```

**Also update the caller** in `crates/agentos-cli/src/main.rs` (find the match arm for `WebCommands::Serve`) to stop passing the removed `vault_passphrase` field.

### 2. Wire graceful shutdown with CancellationToken

**File:** `crates/agentos-cli/src/commands/web.rs`

Replace the `tokio::select!` block (lines 56-59) with `CancellationToken`-based shutdown. The current code:

```rust
// CURRENT (problematic):
tokio::select! {
    result = kernel.run() => { result?; }
    result = server.start() => { result?; }
}
```

When one branch completes, the other is cancelled by being dropped. This means: if the kernel panics, the web server keeps running with a dead kernel; if the web server stops, the kernel is dropped mid-execution without cleanup.

Replace with:

```rust
use tokio_util::sync::CancellationToken;

let shutdown_token = CancellationToken::new();

// Spawn kernel run loop
let kernel_handle = {
    let kernel = kernel.clone();
    let token = shutdown_token.clone();
    tokio::spawn(async move {
        tokio::select! {
            result = kernel.run() => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "Kernel exited with error");
                }
                token.cancel();
            }
            _ = token.cancelled() => {
                tracing::info!("Kernel received shutdown signal");
            }
        }
    })
};

// Spawn web server with graceful shutdown
let server_handle = {
    let token = shutdown_token.clone();
    tokio::spawn(async move {
        if let Err(e) = server.start_with_shutdown(token.clone()).await {
            tracing::error!(error = %e, "Web server exited with error");
        }
        token.cancel();
    })
};

// Wait for Ctrl+C or either task to finish
tokio::select! {
    _ = tokio::signal::ctrl_c() => {
        tracing::info!("Ctrl+C received, shutting down...");
        shutdown_token.cancel();
    }
    _ = shutdown_token.cancelled() => {
        tracing::info!("Component exited, shutting down...");
    }
}

// Wait for both to finish cleanly
let _ = tokio::join!(kernel_handle, server_handle);
```

**File:** `crates/agentos-web/src/server.rs`

Add a `start_with_shutdown` method to `WebServer`. The current `start` method (line 20) calls `axum::serve(listener, app).await` with no shutdown signal. Add:

```rust
use tokio_util::sync::CancellationToken;

impl WebServer {
    // ... existing new() and start() methods ...

    pub async fn start_with_shutdown(self, shutdown: CancellationToken) -> Result<(), anyhow::Error> {
        let app = build_router(self.state);
        let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
        tracing::info!("Web UI listening on http://{}", self.bind_addr);
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await?;
        Ok(())
    }
}
```

**Dependencies to add:** Add `tokio-util` to both Cargo.toml files if not already present:

In `crates/agentos-web/Cargo.toml`:
```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

In `crates/agentos-cli/Cargo.toml`:
```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

### 3. Fix static file path to be CWD-independent

**File:** `crates/agentos-web/src/router.rs`

Replace line 31:

```rust
// CURRENT (breaks outside workspace root):
.nest_service("/static", ServeDir::new("crates/agentos-web/static"))
```

With a path resolved at compile time using `env!("CARGO_MANIFEST_DIR")`:

```rust
.nest_service(
    "/static",
    ServeDir::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static")
    ),
)
```

`CARGO_MANIFEST_DIR` is set by Cargo at compile time to the directory containing `Cargo.toml` for the crate being compiled. For `agentos-web`, this resolves to `crates/agentos-web/`, so `.join("static")` produces `crates/agentos-web/static` as an absolute path.

**Limitation:** This only works for binaries compiled via `cargo build` where the source tree is present at the resolved path. For release deployments where the binary is relocated, a runtime config option (`[web] static_dir`) or embedded static files would be needed as a follow-up.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-cli/src/commands/web.rs` | Remove `--vault_passphrase` arg; remove param from `handle_serve`; add `CancellationToken` shutdown logic |
| `crates/agentos-cli/src/main.rs` | Update `handle_serve` call site to remove passphrase argument |
| `crates/agentos-web/src/server.rs` | Add `start_with_shutdown(CancellationToken)` method |
| `crates/agentos-web/src/router.rs` | Replace relative static path with `env!("CARGO_MANIFEST_DIR")` |
| `crates/agentos-web/Cargo.toml` | Add `tokio-util` dependency |
| `crates/agentos-cli/Cargo.toml` | Add `tokio-util` dependency (if not present) |

---

## Dependencies

None -- this phase has no prerequisites and no other phase depends on it.

---

## Test Plan

1. **Compile check:** `cargo build -p agentos-cli -p agentos-web` must pass.

2. **CLI arg removal verification:** `cargo run -p agentos-cli -- web serve --help` should NOT list `--vault-passphrase` or `--vault_passphrase` as an option.

3. **Graceful shutdown test:** Start the web server in background. Send `SIGINT` (Ctrl+C). Verify both kernel and server exit cleanly within 5 seconds. Check logs for "shutting down" messages, not "panic" or "dropped" warnings.

4. **Static file test:** Run the binary from a directory other than the workspace root (e.g., `/tmp`). Verify that `GET /static/css/pico.min.css` (or whatever static file exists) returns 200, not 404.

5. **Passphrase not in process list:** Start server using env var:
   ```bash
   AGENTOS_VAULT_PASSPHRASE=test agentctl web serve &
   PID=$!
   cat /proc/$PID/cmdline | tr '\0' ' '
   # Should NOT contain "test" or any passphrase value
   ```

---

## Verification

```bash
# Must compile
cargo build -p agentos-cli -p agentos-web

# Tests pass
cargo test -p agentos-cli -p agentos-web

# Verify CLI arg removed
cargo run -p agentos-cli -- web serve --help 2>&1 | grep -ci "vault.passphrase"
# Expected: 0

# Verify static path uses CARGO_MANIFEST_DIR
grep -n "CARGO_MANIFEST_DIR" crates/agentos-web/src/router.rs
# Expected: 1 match on the ServeDir line

# Verify CancellationToken is used
grep -n "CancellationToken" crates/agentos-cli/src/commands/web.rs
grep -n "start_with_shutdown" crates/agentos-web/src/server.rs
```

---

## Related

- [[WebUI Security Fixes Plan]] -- Master plan
- [[WebUI Security Fixes Data Flow]] -- Flow diagrams
