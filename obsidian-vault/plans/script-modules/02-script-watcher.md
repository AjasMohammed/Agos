---
title: Phase 2 — ScriptWatcher (inotify-driven auto-registration)
tags:
  - kernel
  - scripting
  - inotify
  - phase-2
date: 2026-04-14
status: planned
effort: 1d
priority: high
---

# Phase 2 — ScriptWatcher

> Watch `$data_dir/scripts/` via OS-native inotify. On file create/modify → parse + register. On file delete → unregister. Zero polling, zero commands, sub-second latency.

---

## Why This Phase

This is the "drop = install" mechanism. Without it, the user still has to run a command after writing the script. The watcher turns the filesystem into a live, reactive registry.

---

## Current → Target State

**Current:** `ConfigWatcher` already watches config files via `notify`. `ToolRunner::register_dynamic` exists. These are not connected to scripts.

**Target:** `ScriptWatcher` combines them: watches `scripts/`, parses annotations, calls `register_dynamic` / `unregister_dynamic` on `ToolRunner`.

---

## Files to Create

| File | Purpose |
|---|---|
| `crates/agentos-kernel/src/script_watcher.rs` | `ScriptWatcher` implementation |

## Files to Modify

| File | Change |
|---|---|
| `crates/agentos-kernel/src/lib.rs` | `pub mod script_watcher;` |

---

## Detailed Subtasks

### 1. `ScriptWatcher` struct

```rust
use agentos_tools::{ToolRunner, script_tool::{ScriptParser, ScriptTool}};
use notify::{RecommendedWatcher, RecursiveMode, Watcher, Event, EventKind};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct ScriptWatcher {
    scripts_dir: PathBuf,
    tool_runner: Arc<ToolRunner>,
    _watcher: RecommendedWatcher, // keep alive
}
```

### 2. `ScriptWatcher::start()`

```rust
impl ScriptWatcher {
    pub fn start(
        scripts_dir: PathBuf,
        tool_runner: Arc<ToolRunner>,
    ) -> Result<Self, AgentOSError> {
        // Create scripts dir if it doesn't exist
        std::fs::create_dir_all(&scripts_dir)?;

        let (tx, mut rx) = mpsc::channel::<notify::Result<Event>>(64);
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.blocking_send(res);
        })?;
        watcher.watch(&scripts_dir, RecursiveMode::NonRecursive)?;

        // Load all existing scripts at startup
        if let Ok(entries) = std::fs::read_dir(&scripts_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    Self::try_register(&tool_runner, &path);
                }
            }
        }

        let runner_clone = tool_runner.clone();
        tokio::spawn(async move {
            while let Some(event_result) = rx.recv().await {
                match event_result {
                    Ok(event) => Self::handle_event(&runner_clone, event),
                    Err(e) => tracing::warn!(error = %e, "ScriptWatcher notify error"),
                }
            }
        });

        Ok(Self {
            scripts_dir,
            tool_runner,
            _watcher: watcher,
        })
    }

    fn handle_event(runner: &Arc<ToolRunner>, event: Event) {
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                for path in &event.paths {
                    if path.is_file() {
                        Self::try_register(runner, path);
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in &event.paths {
                    // Derive tool name from file stem and try unregister
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        // Also check if the dynamic tool with this name exists
                        // (the name is in the annotations, but the file is gone —
                        //  we use the stem as a fallback hint, but the runner
                        //  tracks the actual name internally)
                        runner.unregister_dynamic_by_path(path);
                        tracing::info!(path = %path.display(), "ScriptWatcher: script removed");
                    }
                }
            }
            _ => {}
        }
    }

    fn try_register(runner: &Arc<ToolRunner>, path: &std::path::Path) {
        match ScriptParser::parse(path) {
            Ok(Some(annotations)) => {
                let name = annotations.name.clone();
                match ScriptTool::new(path.to_path_buf(), annotations) {
                    Ok(tool) => {
                        runner.register_dynamic(Box::new(tool));
                        tracing::info!(tool_name = %name, path = %path.display(), "ScriptWatcher: registered script tool");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "ScriptWatcher: failed to build ScriptTool");
                    }
                }
            }
            Ok(None) => {
                // No @agentos tool: annotation — silently skip
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "ScriptWatcher: parse error");
            }
        }
    }
}
```

### 3. `unregister_dynamic_by_path` on `ToolRunner`

Add a method to `ToolRunner` that removes a dynamic tool registered from a specific path:

```rust
// In runner.rs
pub fn unregister_dynamic_by_path(&self, path: &Path) {
    let mut map = self.dynamic_tools.write().unwrap();
    map.retain(|_name, tool| {
        // ScriptTool exposes its path; other dynamic tools are never path-removed
        if let Some(script) = tool.as_any().downcast_ref::<ScriptTool>() {
            script.script_path() != path
        } else {
            true
        }
    });
}
```

This requires adding `as_any()` to `AgentTool` (or a parallel trait). Simpler: just track `path → name` in `ScriptWatcher` itself:

```rust
// Alternative: ScriptWatcher tracks its own path→name map
path_to_name: Arc<std::sync::Mutex<HashMap<PathBuf, String>>>,
```

When a script is registered, record `path → tool_name`. When a file is deleted, look up the name and call `runner.unregister_dynamic(name)`.

---

## Debounce

Rapid file saves (editor writes tmp file then renames) can fire multiple events. Debounce with a 200ms delay:

```rust
// After receiving a Create/Modify event, wait 200ms before re-parsing
// to let the editor finish writing. Use a HashMap<PathBuf, Instant>
// to track "last event" and only process after the debounce window.
```

Or use the `notify-debouncer-mini` crate (already evaluating with ConfigWatcher).

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel script_watcher

# Manual integration test:
# 1. Start kernel
# 2. cat > $DATA_DIR/scripts/hello.sh
# 3. agentos tool list  → should show "hello"
# 4. rm $DATA_DIR/scripts/hello.sh
# 5. agentos tool list  → "hello" gone
```

---

## Dependencies

- `notify` crate — already in `agentos-kernel/Cargo.toml` (used by `ConfigWatcher`)
- Phase 1 complete (`ScriptParser`, `ScriptTool` available)

---

## Test Plan

| Test | Assertion |
|---|---|
| `watcher_registers_on_create` | New file in scripts/ appears in `list_dynamic_tools()` |
| `watcher_reloads_on_modify` | Editing annotations updates the tool description |
| `watcher_unregisters_on_delete` | Deleting file removes tool from runner |
| `watcher_skips_non_annotated` | File without `@agentos tool:` not registered |
| `watcher_boot_scan` | Scripts already in directory at startup are loaded |
| `watcher_name_conflict` | Script with name matching a static tool logs warning, not registered |
