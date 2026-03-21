---
title: "Phase 01: Single-Tool Factory Function"
tags:
  - kernel
  - sandbox
  - tools
  - v3
  - plan
date: 2026-03-21
status: planned
effort: 1d
priority: critical
---

# Phase 01: Single-Tool Factory Function

> Add a `build_single_tool()` function to `agentos-tools` that constructs exactly one tool by name, loading only the dependencies that specific tool requires.

---

## Why This Phase

The sandbox child process currently calls `ToolRunner::new(&data_dir)` which eagerly constructs all 35+ tools, initializes the fastembed ML model (~23 MB), and opens 3 SQLite databases. For a tool like `datetime` (zero dependencies, 4 MB declared memory), this wastes >1 GiB of virtual address space and hundreds of milliseconds.

A factory function that builds a single tool by name eliminates this waste. It is the foundational building block: Phase 02 wires it into `run_sandbox_exec()`, Phase 03 adjusts RLIMIT_AS based on tool category.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Sandbox tool init | `ToolRunner::new(data_dir)` -- all 35+ tools | `build_single_tool("datetime", data_dir)` -- 1 tool |
| Embedder init | Always (via `ToolRunner::new`) | Only for memory-category tools |
| SQLite stores | Always 3 DBs opened | Only for memory-category tools |
| Factory location | Does not exist | `crates/agentos-tools/src/factory.rs` |
| Factory export | N/A | `pub use factory::{build_single_tool, build_single_tool_with_model_cache, tool_category, ToolCategory};` in lib.rs |

---

## What to Do

### 1. Create `crates/agentos-tools/src/factory.rs`

This file provides a standalone function that constructs a single tool by name. Tools are grouped by dependency category:

```rust
use crate::traits::AgentTool;
use agentos_memory::{Embedder, EpisodicStore, ProceduralStore, SemanticStore};
use agentos_types::AgentOSError;
use std::path::Path;
use std::sync::Arc;

/// Tool dependency category, determined by what the tool needs at construction time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    /// No external dependencies. Constructor takes no args.
    Stateless,
    /// Needs Embedder + SQLite memory stores.
    Memory,
    /// Needs a reqwest HTTP client (constructed internally).
    Network,
    /// Needs HAL references (stubbed in sandbox).
    Hal,
}

/// Determine the dependency category for a tool by name.
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    match name {
        // Stateless tools
        "datetime" | "think" | "file-reader" | "file-writer" | "file-editor"
        | "file-glob" | "file-grep" | "file-delete" | "file-move" | "file-diff"
        | "data-parser" | "memory-block-write" | "memory-block-read"
        | "memory-block-list" | "memory-block-delete" => Some(ToolCategory::Stateless),
        // Memory tools
        "memory-search" | "memory-write" | "memory-read" | "memory-delete"
        | "memory-stats" | "archival-insert" | "archival-search" | "episodic-list"
        | "procedure-create" | "procedure-delete" | "procedure-list"
        | "procedure-search" => Some(ToolCategory::Memory),
        // Network tools
        "http-client" | "web-fetch" => Some(ToolCategory::Network),
        // HAL tools
        "hardware-info" | "sys-monitor" | "process-manager" | "log-reader"
        | "network-monitor" => Some(ToolCategory::Hal),
        // Kernel-context tools (cannot run in sandbox -- need Arc refs)
        "agent-message" | "task-delegate" | "agent-list" | "task-status"
        | "task-list" | "shell-exec" | "agent-manual" | "agent-self" => None,
        _ => None,
    }
}

/// Build a single tool instance by name, loading only the dependencies
/// required for that tool's category.
///
/// Returns `None` if the tool name is unknown or cannot be sandboxed
/// (kernel-context tools like agent-message, task-delegate).
///
/// # Errors
///
/// Returns `AgentOSError::StorageError` if memory store initialization fails.
pub fn build_single_tool(
    name: &str,
    data_dir: &Path,
) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    build_single_tool_with_model_cache(name, data_dir, &data_dir.join("models"))
}

/// Build a single tool with an explicit model cache directory.
pub fn build_single_tool_with_model_cache(
    name: &str,
    data_dir: &Path,
    model_cache_dir: &Path,
) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    let category = match tool_category(name) {
        Some(cat) => cat,
        None => return Ok(None),
    };

    match category {
        ToolCategory::Stateless => build_stateless_tool(name),
        ToolCategory::Memory => build_memory_tool(name, data_dir, model_cache_dir),
        ToolCategory::Network => build_network_tool(name),
        ToolCategory::Hal => build_hal_tool(name),
    }
}

fn build_stateless_tool(name: &str) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    let tool: Box<dyn AgentTool> = match name {
        "datetime" => Box::new(crate::datetime::DatetimeTool::new()),
        "think" => Box::new(crate::think::ThinkTool::new()),
        "file-reader" => Box::new(crate::file_reader::FileReader::new()),
        "file-writer" => Box::new(crate::file_writer::FileWriter::new()),
        "file-editor" => Box::new(crate::file_editor::FileEditor::new()),
        "file-glob" => Box::new(crate::file_glob::FileGlob::new()),
        "file-grep" => Box::new(crate::file_grep::FileGrep::new()),
        "file-delete" => Box::new(crate::file_delete::FileDelete::new()),
        "file-move" => Box::new(crate::file_move::FileMove::new()),
        "file-diff" => Box::new(crate::file_diff::FileDiff::new()),
        "data-parser" => Box::new(crate::data_parser::DataParser::new()),
        "memory-block-write" => Box::new(crate::memory_block_write::MemoryBlockWriteTool::new()),
        "memory-block-read" => Box::new(crate::memory_block_read::MemoryBlockReadTool::new()),
        "memory-block-list" => Box::new(crate::memory_block_list::MemoryBlockListTool::new()),
        "memory-block-delete" => Box::new(crate::memory_block_delete::MemoryBlockDeleteTool::new()),
        _ => return Ok(None),
    };
    Ok(Some(tool))
}

fn build_memory_tool(
    name: &str,
    data_dir: &Path,
    model_cache_dir: &Path,
) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    match name {
        "episodic-list" => {
            let episodic = Arc::new(EpisodicStore::open(data_dir)?);
            return Ok(Some(Box::new(crate::episodic_list::EpisodicList::new(episodic))));
        }
        "procedure-create" | "procedure-delete" | "procedure-list" | "procedure-search" => {
            let embedder = init_embedder(model_cache_dir)?;
            let procedural = Arc::new(ProceduralStore::open_with_embedder(data_dir, embedder)?);
            let tool: Box<dyn AgentTool> = match name {
                "procedure-create" => Box::new(crate::procedure_create::ProcedureCreate::new(procedural)),
                "procedure-delete" => Box::new(crate::procedure_delete::ProcedureDelete::new(procedural)),
                "procedure-list" => Box::new(crate::procedure_list::ProcedureList::new(procedural)),
                "procedure-search" => Box::new(crate::procedure_search::ProcedureSearch::new(procedural)),
                _ => unreachable!(),
            };
            return Ok(Some(tool));
        }
        "memory-read" | "archival-insert" | "archival-search" => {
            let embedder = init_embedder(model_cache_dir)?;
            let semantic = Arc::new(SemanticStore::open_with_embedder(data_dir, embedder)?);
            let tool: Box<dyn AgentTool> = match name {
                "memory-read" => Box::new(crate::memory_read::MemoryRead::new(semantic)),
                "archival-insert" => Box::new(crate::archival_insert::ArchivalInsert::new(semantic)),
                "archival-search" => Box::new(crate::archival_search::ArchivalSearch::new(semantic)),
                _ => unreachable!(),
            };
            return Ok(Some(tool));
        }
        _ => {}
    }

    let embedder = init_embedder(model_cache_dir)?;
    let semantic = Arc::new(SemanticStore::open_with_embedder(data_dir, embedder.clone())?);
    let episodic = Arc::new(EpisodicStore::open(data_dir)?);

    let tool: Box<dyn AgentTool> = match name {
        "memory-search" => Box::new(crate::memory_search::MemorySearch::new(semantic, episodic)),
        "memory-write" => Box::new(crate::memory_write::MemoryWrite::new(semantic, episodic)),
        "memory-delete" => Box::new(crate::memory_delete::MemoryDelete::new(semantic, episodic)),
        "memory-stats" => {
            let procedural = Arc::new(ProceduralStore::open_with_embedder(data_dir, embedder)?);
            Box::new(crate::memory_stats::MemoryStats::new(semantic, episodic, procedural))
        }
        _ => return Ok(None),
    };
    Ok(Some(tool))
}

fn build_network_tool(name: &str) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    let tool: Box<dyn AgentTool> = match name {
        "http-client" => {
            Box::new(crate::http_client::HttpClientTool::new().map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "http-client".to_string(),
                    reason: format!("Failed to init HTTP client in sandbox: {}", e),
                }
            })?)
        }
        "web-fetch" => {
            Box::new(crate::web_fetch::WebFetch::new().map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "web-fetch".to_string(),
                    reason: format!("Failed to init web-fetch in sandbox: {}", e),
                }
            })?)
        }
        _ => return Ok(None),
    };
    Ok(Some(tool))
}

fn build_hal_tool(name: &str) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    let tool: Box<dyn AgentTool> = match name {
        "hardware-info" => Box::new(crate::hardware_info::HardwareInfoTool::new()),
        "sys-monitor" => Box::new(crate::sys_monitor::SysMonitorTool::new()),
        "process-manager" => Box::new(crate::process_manager::ProcessManagerTool::new()),
        "log-reader" => Box::new(crate::log_reader::LogReaderTool::new()),
        "network-monitor" => Box::new(crate::network_monitor::NetworkMonitorTool::new()),
        _ => return Ok(None),
    };
    Ok(Some(tool))
}
```

### 2. Register the module in `crates/agentos-tools/src/lib.rs`

Add near the top of the file:

```rust
pub mod factory;
```

And add a re-export:

```rust
pub use factory::{build_single_tool, tool_category, ToolCategory};
```

### 3. Add unit tests to `factory.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_stateless_tools_build_without_data_dir() {
        let tmp = TempDir::new().unwrap();
        for name in ["datetime", "think", "file-reader", "file-writer",
                     "file-editor", "file-glob", "file-grep", "file-delete",
                     "file-move", "file-diff", "data-parser", "memory-block-write",
                     "memory-block-read", "memory-block-list", "memory-block-delete"] {
            let result = build_single_tool(name, tmp.path());
            assert!(result.is_ok(), "Failed to build {}: {:?}", name, result.err());
            let tool = result.unwrap();
            assert!(tool.is_some(), "Tool {} returned None", name);
            assert_eq!(tool.unwrap().name(), name);
        }
    }

    #[test]
    fn test_category_classification() {
        assert_eq!(tool_category("datetime"), Some(ToolCategory::Stateless));
        assert_eq!(tool_category("memory-block-write"), Some(ToolCategory::Stateless));
        assert_eq!(tool_category("memory-search"), Some(ToolCategory::Memory));
        assert_eq!(tool_category("web-fetch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("hardware-info"), Some(ToolCategory::Hal));
        assert_eq!(tool_category("network-monitor"), Some(ToolCategory::Hal));
        assert_eq!(tool_category("agent-message"), None); // kernel-context
        assert_eq!(tool_category("agent-manual"), None); // kernel-context
        assert_eq!(tool_category("nonexistent"), None);
    }

    #[test]
    fn test_kernel_context_tools_return_none() {
        let tmp = TempDir::new().unwrap();
        for name in ["agent-message", "task-delegate", "agent-list",
                     "task-status", "task-list", "shell-exec", "agent-manual",
                     "agent-self"] {
            let result = build_single_tool(name, tmp.path()).unwrap();
            assert!(result.is_none(), "Kernel-context tool {} should return None", name);
        }
    }

    #[test]
    fn test_memory_block_tools_are_stateless() {
        let tmp = TempDir::new().unwrap();
        for name in ["memory-block-write", "memory-block-read",
                     "memory-block-list", "memory-block-delete"] {
            let result = build_single_tool(name, tmp.path());
            assert!(result.is_ok(), "Failed to build {}: {:?}", name, result.err());
            assert!(result.unwrap().is_some(), "Tool {} returned None", name);
        }
    }
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/factory.rs` | New file: `build_single_tool()`, `build_single_tool_with_model_cache()`, `tool_category()`, `ToolCategory` enum |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod factory;` and re-export the factory helpers + category API |

---

## Prerequisites

None -- this is the first phase.

---

## Test Plan

- `cargo test -p agentos-tools -- factory` must pass
- `test_stateless_tools_build_without_data_dir`: all 15 stateless tools build successfully with a temp dir
- `test_category_classification`: tool_category returns correct category for known tools, None for unknown
- `test_kernel_context_tools_return_none`: tools that need kernel Arc refs return None
- `test_memory_block_tools_are_stateless`: memory-block-* tools build without embedder

---

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- factory --nocapture
cargo clippy -p agentos-tools -- -D warnings
```
