//! AgentOS WASM tool execution engine.
//!
//! This crate provides [`WasmToolExecutor`] which loads `.wasm` modules
//! and exposes them as [`AgentTool`](agentos_tools::AgentTool) implementations,
//! slotting them transparently into the existing `ToolRunner`.

mod wasm_tool;

pub use wasm_tool::WasmTool;

use std::path::Path;
use std::sync::Arc;
use wasmtime::{Config, Engine};

/// Shared Wasmtime engine. Expensive to create — build once at kernel boot
/// and store as `Arc<WasmToolExecutor>`.
pub struct WasmToolExecutor {
    engine: Arc<Engine>,
    /// Root directory where AGENTOS_OUTPUT_FILE temp files are written.
    data_dir: std::path::PathBuf,
}

impl WasmToolExecutor {
    /// Create a new executor. Called once during kernel boot.
    pub fn new(data_dir: impl Into<std::path::PathBuf>) -> Result<Self, anyhow::Error> {
        let mut cfg = Config::new();
        cfg.async_support(true);
        // Epoch-based interruption lets us enforce max_cpu_ms without busy-polling.
        cfg.epoch_interruption(true);
        let engine = Engine::new(&cfg)?;
        Ok(Self {
            engine: Arc::new(engine),
            data_dir: data_dir.into(),
        })
    }

    /// Pre-compile a `.wasm` file into a [`WasmTool`].
    pub fn load(
        &self,
        manifest: &agentos_types::ToolManifest,
        wasm_path: &Path,
    ) -> Result<WasmTool, anyhow::Error> {
        let tool = WasmTool::load(Arc::clone(&self.engine), manifest, wasm_path)?;
        Ok(tool.with_data_dir(self.data_dir.clone()))
    }
}
