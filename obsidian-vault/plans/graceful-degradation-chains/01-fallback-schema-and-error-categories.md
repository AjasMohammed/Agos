---
title: "Phase 1: Fallback Schema & Error Categories"
tags:
  - kernel
  - tools
  - resilience
  - v4
  - plan
date: 2026-04-07
status: complete
effort: 1d
priority: high
---

# Phase 1: Fallback Schema & Error Categories

> Define the manifest schema for fallback chains and categorize `AgentOSError` variants for stable matching.

---

## Why This Phase

Fallback chains need two foundations: (1) a way to declare them in tool manifests, and (2) a stable error categorization so fallbacks match on error *type*, not error *message*. This phase establishes both, enabling the kernel resolver (Phase 2) and core declarations (Phase 3).

---

## Current → Target State

**Current:** Tool manifests have no fallback fields. `AgentOSError` has 30+ variants but no categorization method — code that wants to check "is this a storage error?" must match the specific variant.

**Target:** Tool manifests support an optional `[[fallback]]` array. `AgentOSError` gains an `error_category()` method returning a stable string key for fallback matching.

---

## Detailed Subtasks

### 1. Define fallback schema in tool manifest parsing

**File:** `crates/agentos-kernel/src/tool_registry.rs`

The existing `ToolManifest` struct (parsed from TOML) needs a new optional field:

```rust
#[serde(default)]
pub fallbacks: Vec<FallbackRule>,
```

Where `FallbackRule` is:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackRule {
    /// Error category to match (e.g., "StorageError", "NetworkError")
    pub on_error: String,
    /// Tool to try as fallback
    pub try_tool: String,
    /// Payload key transformations
    #[serde(default)]
    pub transform: HashMap<String, String>,
    /// Max retries for this specific fallback (default 1)
    #[serde(default = "default_max_retries")]
    pub max_retries: u8,
}

fn default_max_retries() -> u8 { 1 }
```

Example TOML:

```toml
[[fallback]]
on_error = "StorageError"
try_tool = "file-write"
transform = { path = "prepend:/tmp/overflow/" }
max_retries = 1

[[fallback]]
on_error = "PermissionDenied"
try_tool = "notify-user"
transform = { message = "replace:Permission denied for file operation, requesting user assistance" }
```

### 2. Add `error_category()` to AgentOSError

**File:** `crates/agentos-types/src/error.rs`

Add a method that returns a stable string key for each error variant:

```rust
impl AgentOSError {
    /// Stable category key for fallback chain matching.
    /// Returns the variant name as a string, e.g., "StorageError", "PermissionDenied".
    pub fn error_category(&self) -> &'static str {
        match self {
            Self::StorageError(_) => "StorageError",
            Self::PermissionDenied { .. } => "PermissionDenied",
            Self::ToolExecutionFailed(_) => "ToolExecutionFailed",
            Self::NetworkError(_) => "NetworkError",
            Self::SchemaValidation(_) => "SchemaValidation",
            Self::Timeout(_) => "Timeout",
            Self::ToolNotFound(_) => "ToolNotFound",
            Self::ToolBlocked(_) => "ToolBlocked",
            // ... all other variants
            _ => "Unknown",
        }
    }
}
```

### 3. Define payload transform operations

**File:** `crates/agentos-types/src/fallback.rs` (new file, re-exported from lib.rs)

```rust
/// Supported payload transform operations for fallback chains.
#[derive(Debug, Clone)]
pub enum TransformOp {
    /// Prepend a string to the value: "prepend:/tmp/"
    Prepend(String),
    /// Append a string to the value: "append:.bak"
    Append(String),
    /// Replace the entire value: "replace:new_value"
    Replace(String),
    /// Set a default if the key is missing: "default:fallback_value"
    Default(String),
}

impl TransformOp {
    /// Parse a transform string like "prepend:/tmp/" into an operation.
    pub fn parse(s: &str) -> Result<Self, String> {
        let (op, value) = s.split_once(':')
            .ok_or_else(|| format!("Invalid transform: {s}"))?;
        match op {
            "prepend" => Ok(Self::Prepend(value.to_string())),
            "append" => Ok(Self::Append(value.to_string())),
            "replace" => Ok(Self::Replace(value.to_string())),
            "default" => Ok(Self::Default(value.to_string())),
            _ => Err(format!("Unknown transform op: {op}")),
        }
    }

    /// Apply this transform to a JSON string value.
    pub fn apply(&self, current: Option<&str>) -> String {
        match self {
            Self::Prepend(prefix) => format!("{}{}", prefix, current.unwrap_or("")),
            Self::Append(suffix) => format!("{}{}", current.unwrap_or(""), suffix),
            Self::Replace(new_val) => new_val.clone(),
            Self::Default(default) => current.unwrap_or(default).to_string(),
        }
    }
}
```

### 4. Add `FallbackRule` to `ToolManifest` type

**File:** `crates/agentos-types/src/tool.rs`

Add the `FallbackRule` struct here (not in the kernel) so both the kernel and tools crate can reference it:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackRule {
    pub on_error: String,
    pub try_tool: String,
    #[serde(default)]
    pub transform: std::collections::HashMap<String, String>,
    #[serde(default = "default_max_retries")]
    pub max_retries: u8,
}

fn default_max_retries() -> u8 { 1 }
```

Add to `ToolManifest`:

```rust
#[serde(default)]
pub fallbacks: Vec<FallbackRule>,
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/tool.rs` | Add `FallbackRule` struct, add `fallbacks` field to `ToolManifest` |
| `crates/agentos-types/src/error.rs` | Add `error_category()` method to `AgentOSError` |
| `crates/agentos-types/src/fallback.rs` | New file: `TransformOp` enum with parse/apply |
| `crates/agentos-types/src/lib.rs` | Add `pub mod fallback; pub use fallback::*;` |

---

## Dependencies

- **Requires:** None
- **Blocks:** Phase 2 (kernel resolver), Phase 3 (core declarations)

---

## Test Plan

1. **Manifest parsing test** — parse a TOML manifest with `[[fallback]]` sections; verify `FallbackRule` fields are populated correctly
2. **Manifest without fallbacks** — parse an existing manifest with no fallback sections; verify `fallbacks` is empty (backward compat)
3. **Error category test** — verify `error_category()` returns correct strings for `StorageError`, `PermissionDenied`, `NetworkError`, `Timeout`
4. **Transform parse test** — verify `TransformOp::parse("prepend:/tmp/")` returns `Prepend("/tmp/")`
5. **Transform apply test** — verify `Prepend("/tmp/").apply(Some("file.txt"))` returns `"/tmp/file.txt"`
6. **Transform apply default test** — verify `Default("fallback").apply(None)` returns `"fallback"`

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-types -- fallback
cargo test -p agentos-types -- error_category
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
