---
title: "Phase 03 — Web UI Integration"
tags:
  - web
  - cli
  - next-steps
  - v3
date: 2026-03-17
status: complete
effort: 4h
priority: medium
---

# Phase 03 — Web UI Integration

> Wire the existing `agentos-web` crate into the system by adding an `agentctl web serve` CLI command that boots the kernel in-process and starts the Axum+HTMX web server.

---

## Why This Phase

The `agentos-web` crate at `crates/agentos-web/` is a fully implemented Axum+HTMX web UI server with:
- `WebServer` struct (`src/server.rs`) that takes `Arc<Kernel>` and a bind address
- Router with routes for dashboard, tasks, tools, pipelines, audit, agents, secrets (`src/router.rs`)
- Handler modules for each page (`src/handlers/*.rs`)
- Template engine using minijinja (`src/templates.rs`)
- App state holding `Arc<Kernel>` and templates (`src/state.rs`)

However, this crate is completely disconnected from the rest of the system:
- `agentos-kernel` does not depend on `agentos-web`
- `agentos-cli` does not depend on `agentos-web`
- There is no CLI command to start the web server
- There is no standalone binary in the crate
- The workspace `Cargo.toml` includes it but nothing references it

The web UI is dead code that will bitrot.

---

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Web UI access | Not possible | `agentctl web serve [--port PORT]` starts server |
| CLI dependency on agentos-web | None | `agentos-cli/Cargo.toml` depends on `agentos-web` |
| Web CLI command group | Does not exist | `agentctl web serve` subcommand with `--port` flag |
| Kernel boot for web | Not applicable | In-process kernel boot (same as `agentctl kernel boot`) |

---

## What to Do

### Step 1: Add `agentos-web` dependency to `agentos-cli`

Open `crates/agentos-cli/Cargo.toml` and add:

```toml
[dependencies]
# ... existing deps ...
agentos-web = { path = "../agentos-web" }
```

### Step 2: Create the web CLI command module

Create `crates/agentos-cli/src/commands/web.rs`:

```rust
use clap::Args;

#[derive(Args, Debug)]
pub struct WebArgs {
    #[command(subcommand)]
    pub command: WebCommand,
}

#[derive(clap::Subcommand, Debug)]
pub enum WebCommand {
    /// Start the web UI server
    Serve {
        /// Port to bind the web server on
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Host/IP to bind on
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
}
```

### Step 3: Implement the serve handler

The `serve` command needs to boot a kernel in-process (similar to `agentctl kernel boot`) and then start the web server. The implementation should be in the same file or a dedicated handler:

```rust
use agentos_kernel::Kernel;
use agentos_web::WebServer;
use std::net::SocketAddr;
use std::sync::Arc;

pub async fn handle_web_serve(host: &str, port: u16) -> anyhow::Result<()> {
    // Boot the kernel in-process
    let kernel = Kernel::boot().await?;
    let kernel = Arc::new(kernel);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;
    tracing::info!("Starting web UI on http://{}", addr);

    let server = WebServer::new(addr, kernel.clone());

    // Start the web server (blocks until shutdown)
    server.start().await?;

    Ok(())
}
```

### Step 4: Register the web command group in the CLI

Open `crates/agentos-cli/src/commands/mod.rs` and add:

```rust
pub mod web;
```

Open `crates/agentos-cli/src/main.rs` (or wherever the clap `App` / `Command` enum is defined) and add:

```rust
/// Web UI commands
Web(commands::web::WebArgs),
```

In the command dispatch match:

```rust
Commands::Web(args) => match args.command {
    commands::web::WebCommand::Serve { port, host } => {
        commands::web::handle_web_serve(&host, port).await?;
    }
},
```

### Step 5: Verify the web server starts

Run `agentctl web serve --port 8080` and confirm:
1. Kernel boots successfully
2. Web server binds to the port
3. HTTP GET to `http://127.0.0.1:8080/` returns the dashboard HTML
4. No compilation errors

### Step 6: Add a note about kernel boot sharing (optional)

If the kernel is already running via `agentctl kernel boot` and the user wants to add the web UI, the current approach boots a second kernel instance. A future enhancement could connect to the existing kernel via the bus. Document this limitation in a comment.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-cli/Cargo.toml` | Add `agentos-web = { path = "../agentos-web" }` dependency |
| `crates/agentos-cli/src/commands/web.rs` | New file: `WebArgs`, `WebCommand::Serve`, `handle_web_serve()` |
| `crates/agentos-cli/src/commands/mod.rs` | Add `pub mod web;` |
| `crates/agentos-cli/src/main.rs` | Add `Web(WebArgs)` variant to `Commands` enum, dispatch to handler |

---

## Prerequisites

None -- this phase is independent. The `agentos-web` crate already compiles and has no missing functionality.

---

## Test Plan

1. **Compilation test:**
   - `cargo build -p agentos-cli` must compile with the new dependency
   - `cargo build -p agentos-web` must still compile independently

2. **Command registration test:**
   - `agentctl web --help` should show the `serve` subcommand
   - `agentctl web serve --help` should show `--port` and `--host` flags

3. **Integration test (manual):**
   - Start: `agentctl web serve --port 9090`
   - Verify: `curl http://127.0.0.1:9090/` returns HTML with "AgentOS" in the body
   - Verify: The server shuts down cleanly on Ctrl+C

4. **Existing test regression:**
   - `cargo test -p agentos-cli` must still pass
   - `cargo test -p agentos-web` must still pass

---

## Verification

```bash
cargo build -p agentos-cli
cargo test -p agentos-cli
cargo test -p agentos-web
# Manual smoke test:
cargo run -p agentos-cli -- web serve --port 9090 &
sleep 2
curl -s http://127.0.0.1:9090/ | head -5
kill %1
```

---

## Related

- [[Unwired Features Plan]] -- Parent plan
- [[22-Unwired Features]] -- Next-steps parent index
