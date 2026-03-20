---
title: Define ManualSection Enum and Query Types
tags:
  - tools
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 2h
priority: high
---

# Define ManualSection Enum and Query Types

> Create the `agent_manual.rs` module with the `ManualSection` enum, `ToolSummary` struct, and `AgentManualTool` struct skeleton.

---

## Why This Subtask

This is the foundation for the entire agent-manual tool. It defines the data types that all other subtasks build on. The enum determines what sections exist; the `ToolSummary` struct determines what tool data is available for dynamic sections. Everything else is a function of these types.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `agent_manual` module | Does not exist | New file `crates/agentos-tools/src/agent_manual.rs` |
| `ManualSection` enum | N/A | 9-variant enum: `Index`, `Tools`, `ToolDetail`, `Permissions`, `Memory`, `Events`, `Commands`, `Errors`, `Feedback` |
| `ToolSummary` struct | N/A | Lightweight struct with `name`, `description`, `version`, `permissions`, `input_schema`, `trust_tier` |
| `AgentManualTool` struct | N/A | Struct holding `Vec<ToolSummary>`, implements `AgentTool` trait (skeleton only) |

---

## What to Do

### 1. Create `crates/agentos-tools/src/agent_manual.rs`

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Which section of the agent manual to query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManualSection {
    Index,
    Tools,
    ToolDetail,
    Permissions,
    Memory,
    Events,
    Commands,
    Errors,
    Feedback,
}

impl ManualSection {
    /// Parse from a string. Returns None for unrecognized sections.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "index" => Some(Self::Index),
            "tools" => Some(Self::Tools),
            "tool-detail" => Some(Self::ToolDetail),
            "permissions" => Some(Self::Permissions),
            "memory" => Some(Self::Memory),
            "events" => Some(Self::Events),
            "commands" => Some(Self::Commands),
            "errors" => Some(Self::Errors),
            "feedback" => Some(Self::Feedback),
            _ => None,
        }
    }

    /// All valid section names for the index listing.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "index",
            "tools",
            "tool-detail",
            "permissions",
            "memory",
            "events",
            "commands",
            "errors",
            "feedback",
        ]
    }
}

/// Lightweight summary of a registered tool, injected at construction time.
/// Avoids holding a reference to the live ToolRegistry.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
    pub version: String,
    /// Permission strings from the manifest, e.g. ["fs.user_data:r"]
    pub permissions: Vec<String>,
    /// Optional JSON Schema for the tool's input payload.
    pub input_schema: Option<serde_json::Value>,
    /// Trust tier: "core", "verified", "community"
    pub trust_tier: String,
}

/// The agent-manual tool. Provides queryable OS documentation.
pub struct AgentManualTool {
    tool_summaries: Vec<ToolSummary>,
}

impl AgentManualTool {
    pub fn new(tool_summaries: Vec<ToolSummary>) -> Self {
        Self { tool_summaries }
    }

    /// Build ToolSummary list from a slice of RegisteredTool references.
    /// Called by the kernel/runner when constructing the tool.
    pub fn summaries_from_registry(
        tools: &[&agentos_types::RegisteredTool],
    ) -> Vec<ToolSummary> {
        tools
            .iter()
            .map(|t| ToolSummary {
                name: t.manifest.manifest.name.clone(),
                description: t.manifest.manifest.description.clone(),
                version: t.manifest.manifest.version.clone(),
                permissions: t.manifest.capabilities_required.permissions.clone(),
                input_schema: t.manifest.input_schema.clone(),
                trust_tier: format!("{:?}", t.manifest.manifest.trust_tier).to_lowercase(),
            })
            .collect()
    }
}

#[async_trait]
impl AgentTool for AgentManualTool {
    fn name(&self) -> &str {
        "agent-manual"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // No permissions required — this is read-only public documentation.
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let section_str = payload
            .get("section")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "agent-manual requires 'section' field. Valid sections: index, tools, tool-detail, permissions, memory, events, commands, errors, feedback".into(),
                )
            })?;

        let section = ManualSection::from_str(section_str).ok_or_else(|| {
            AgentOSError::SchemaValidation(format!(
                "Unknown manual section '{}'. Valid sections: {}",
                section_str,
                ManualSection::all_names().join(", ")
            ))
        })?;

        // Dispatch to section-specific generators (implemented in subtask 27-02)
        match section {
            ManualSection::Index => self.section_index(),
            ManualSection::Tools => self.section_tools(),
            ManualSection::ToolDetail => {
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "tool-detail section requires 'name' field".into(),
                        )
                    })?;
                self.section_tool_detail(name)
            }
            ManualSection::Permissions => self.section_permissions(),
            ManualSection::Memory => self.section_memory(),
            ManualSection::Events => self.section_events(),
            ManualSection::Commands => self.section_commands(),
            ManualSection::Errors => self.section_errors(),
            ManualSection::Feedback => self.section_feedback(),
        }
    }
}
```

### 2. Add placeholder methods on `AgentManualTool`

Add these method stubs so the file compiles. Each returns a `todo!()` for now (subtask 27-02 fills them in):

```rust
impl AgentManualTool {
    // ... (existing new/summaries_from_registry methods)

    fn section_index(&self) -> Result<serde_json::Value, AgentOSError> {
        todo!("Implemented in subtask 27-02")
    }

    fn section_tools(&self) -> Result<serde_json::Value, AgentOSError> {
        todo!("Implemented in subtask 27-02")
    }

    fn section_tool_detail(&self, name: &str) -> Result<serde_json::Value, AgentOSError> {
        let _ = name;
        todo!("Implemented in subtask 27-02")
    }

    fn section_permissions(&self) -> Result<serde_json::Value, AgentOSError> {
        todo!("Implemented in subtask 27-02")
    }

    fn section_memory(&self) -> Result<serde_json::Value, AgentOSError> {
        todo!("Implemented in subtask 27-02")
    }

    fn section_events(&self) -> Result<serde_json::Value, AgentOSError> {
        todo!("Implemented in subtask 27-02")
    }

    fn section_commands(&self) -> Result<serde_json::Value, AgentOSError> {
        todo!("Implemented in subtask 27-02")
    }

    fn section_errors(&self) -> Result<serde_json::Value, AgentOSError> {
        todo!("Implemented in subtask 27-02")
    }

    fn section_feedback(&self) -> Result<serde_json::Value, AgentOSError> {
        todo!("Implemented in subtask 27-02")
    }
}
```

**Important:** The `todo!()` stubs are temporary. They let subtask 01 be verified independently (the file compiles, the trait is implemented) while subtask 02 fills in the actual content.

### 3. Register the module in `crates/agentos-tools/src/lib.rs`

Add at the top of `lib.rs`:

```rust
pub mod agent_manual;
```

And add a re-export:

```rust
pub use agent_manual::AgentManualTool;
```

This goes alongside the existing `pub mod` and `pub use` declarations. Do NOT register the tool in `ToolRunner` yet -- that is subtask 03.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/agent_manual.rs` | New file: `ManualSection` enum, `ToolSummary` struct, `AgentManualTool` struct with `AgentTool` impl and placeholder section methods |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod agent_manual;` and `pub use agent_manual::AgentManualTool;` |

---

## Prerequisites

None -- this is the first subtask.

---

## Test Plan

- `cargo build -p agentos-tools` must compile with no errors.
- Add a unit test in `agent_manual.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manual_section_from_str() {
        assert_eq!(ManualSection::from_str("index"), Some(ManualSection::Index));
        assert_eq!(ManualSection::from_str("tools"), Some(ManualSection::Tools));
        assert_eq!(ManualSection::from_str("tool-detail"), Some(ManualSection::ToolDetail));
        assert_eq!(ManualSection::from_str("permissions"), Some(ManualSection::Permissions));
        assert_eq!(ManualSection::from_str("memory"), Some(ManualSection::Memory));
        assert_eq!(ManualSection::from_str("events"), Some(ManualSection::Events));
        assert_eq!(ManualSection::from_str("commands"), Some(ManualSection::Commands));
        assert_eq!(ManualSection::from_str("errors"), Some(ManualSection::Errors));
        assert_eq!(ManualSection::from_str("feedback"), Some(ManualSection::Feedback));
        assert_eq!(ManualSection::from_str("nonexistent"), None);
    }

    #[test]
    fn test_all_names_count() {
        assert_eq!(ManualSection::all_names().len(), 9);
    }

    #[test]
    fn test_summaries_from_registry_empty() {
        let summaries = AgentManualTool::summaries_from_registry(&[]);
        assert!(summaries.is_empty());
    }
}
```

---

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- manual_section --nocapture
cargo clippy -p agentos-tools -- -D warnings
```
