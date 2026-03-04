use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How the tool's logic is executed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExecutorType {
    #[default]
    Inline, // built-in Rust implementation (compiled into kernel)
    Wasm,   // external .wasm module loaded at runtime
}

/// Executor configuration for a tool manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutor {
    #[serde(rename = "type", default)]
    pub executor_type: ExecutorType,
    /// Path to the .wasm file, relative to the manifest's directory.
    pub wasm_path: Option<PathBuf>,
}

impl Default for ToolExecutor {
    fn default() -> Self {
        Self {
            executor_type: ExecutorType::Inline,
            wasm_path: None,
        }
    }
}

/// A tool's manifest, parsed from tool.toml at install time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    pub manifest: ToolInfo,
    pub capabilities_required: ToolCapabilities,
    pub capabilities_provided: ToolOutputs,
    pub intent_schema: ToolSchema,
    pub sandbox: ToolSandbox,
    /// Which execution backend should run this tool. Defaults to Inline.
    #[serde(default)]
    pub executor: ToolExecutor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCapabilities {
    pub permissions: Vec<String>,  // e.g. ["fs.read", "context.write"]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutputs {
    pub outputs: Vec<String>,  // e.g. ["content.text", "content.structured"]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub input: String,   // e.g. "FileReadIntent"
    pub output: String,  // e.g. "FileContent"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSandbox {
    pub network: bool,
    pub fs_write: bool,
    #[serde(default)]
    pub gpu: bool,
    pub max_memory_mb: u64,
    pub max_cpu_ms: u64,
    /// Explicit syscall allowlist override. Empty = use default base allowlist.
    #[serde(default)]
    pub syscalls: Vec<String>,
}

/// A registered tool in the kernel's tool registry.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    pub id: ToolID,
    pub manifest: ToolManifest,
    pub status: ToolStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Available,
    Running,
    Disabled,
}
