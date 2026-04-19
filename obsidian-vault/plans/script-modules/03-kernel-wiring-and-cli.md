---
title: Phase 3 — Kernel Wiring + CLI Commands
tags:
  - kernel
  - cli
  - scripting
  - phase-3
date: 2026-04-14
status: planned
effort: 1d
priority: high
---

# Phase 3 — Kernel Wiring + CLI Commands

> Start `ScriptWatcher` during kernel boot. Add `scripts_dir` to config. Wire `agentos script list` and `agentos script reload` into the CLI.

---

## Files to Modify

| File | Change |
|---|---|
| `config/default.toml` | Add `[tools] scripts_dir = "scripts"` |
| `crates/agentos-types/src/config.rs` | Add `scripts_dir: String` to `ToolsConfig` |
| `crates/agentos-kernel/src/kernel.rs` | Start `ScriptWatcher` after `ToolRunner` init |
| `crates/agentos-bus/src/message.rs` | Add `ListScripts`, `ReloadScript` to `KernelCommand` |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch new commands |
| `crates/agentos-cli/src/commands/mod.rs` | Add `script` subcommand group |
| `crates/agentos-cli/src/commands/script.rs` | New file: `ScriptCommands` enum + handlers |

---

## Config Change

```toml
# config/default.toml
[tools]
core_tools_dir  = "tools/core"
user_tools_dir  = "tools/user"
scripts_dir     = "scripts"       # NEW: relative to data_dir
data_dir        = "/opt/agentos/data"
```

```rust
// agentos-types/src/config.rs (ToolsConfig)
pub struct ToolsConfig {
    pub core_tools_dir: String,
    pub user_tools_dir: String,
    pub scripts_dir: String,      // NEW
    pub data_dir: String,
    // ...
}
```

## Kernel Boot (kernel.rs)

After `ToolRunner` is initialized and static tools are registered, start the watcher:

```rust
// After tool_runner is built:
let scripts_dir = data_dir.join(&config.tools.scripts_dir);
let script_watcher = ScriptWatcher::start(scripts_dir, tool_runner.clone())?;
// Store in Kernel struct to keep it alive:
self.script_watcher = Some(script_watcher);
```

## New KernelCommands

```rust
// agentos-bus/src/message.rs
pub enum KernelCommand {
    // ... existing ...
    ListScripts,
    ReloadScript { name: String },
}
```

## run_loop.rs Dispatch

```rust
KernelCommand::ListScripts => {
    let scripts = tool_runner.list_dynamic_tools()
        .into_iter()
        .filter(|t| t.source == ToolSource::Script)
        .collect::<Vec<_>>();
    respond(BusResponse::ScriptList(scripts));
}
KernelCommand::ReloadScript { name } => {
    // Find script path by name, re-parse, re-register
    if let Some(path) = script_watcher.path_for(&name) {
        script_watcher.force_reload(&path);
        respond(BusResponse::Ok);
    } else {
        respond(BusResponse::NotFound(name));
    }
}
```

## CLI — `crates/agentos-cli/src/commands/script.rs`

```rust
#[derive(Subcommand)]
pub enum ScriptCommands {
    /// List all loaded script tools
    List,
    /// Force reload a script tool by name (re-parse annotations)
    Reload { name: String },
}

pub async fn handle(cmd: ScriptCommands, bus: &BusClient) -> Result<(), AgentOSError> {
    match cmd {
        ScriptCommands::List => {
            let scripts = bus.send(KernelCommand::ListScripts).await?;
            // Print table: name | version | path | permissions
        }
        ScriptCommands::Reload { name } => {
            bus.send(KernelCommand::ReloadScript { name }).await?;
            println!("Script reloaded.");
        }
    }
}
```

---

## Example User Session

```bash
# Write a script
cat > ~/.local/share/agentos/scripts/summarize.py << 'EOF'
#!/usr/bin/env python3
# @agentos tool: summarize
# @description: Summarizes text to 3 bullet points
# @permissions: fs.data:r
import json, os, sys

inp = json.loads(os.environ["AGENTOS_INPUT"])
text = inp.get("text", "")
words = text.split()
# naive summary: first 3 sentences
sentences = text.split(". ")[:3]
print(json.dumps({"summary": sentences}))
EOF

# Immediately visible — no command needed
agentos tool list
# → summarize   0.1.0   script   Summarizes text to 3 bullet points

# Force reload after editing
agentos script reload summarize

# List only script tools
agentos script list
```

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel
cargo test -p agentos-cli
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
