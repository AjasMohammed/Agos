//! `WasmTool` — a WASM-backed implementation of the `AgentTool` trait.
//!
//! ## Execution protocol
//!
//! 1. Kernel generates a unique output file path: `{data_dir}/tool-out/{task_id}-{uuid}.json`
//! 2. WASM stdin  ← JSON payload bytes
//! 3. `AGENTOS_OUTPUT_FILE` env var ← the unique output path
//! 4. Module runs (`_start`) and writes its result to the output file
//! 5. Kernel reads JSON from the output file, deletes it via Drop

use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, ExecutorType, PermissionOp, ToolManifest};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};
use wasmtime::{Engine, Linker, Module, ResourceLimiter, Store};
use wasmtime_wasi::{
    p1::WasiP1Ctx,
    p2::pipe::{MemoryInputPipe, MemoryOutputPipe},
    WasiCtxBuilder,
};

/// Maximum WASM linear memory per module invocation (256 MiB).
const MAX_WASM_MEMORY_BYTES: usize = 256 * 1024 * 1024;

/// Store data that embeds the WASI context together with a memory limit guard.
/// The `ResourceLimiter` impl prevents WASM modules from growing their linear
/// memory beyond `MAX_WASM_MEMORY_BYTES`, protecting against OOM attacks.
struct WasiState {
    ctx: WasiP1Ctx,
}

impl ResourceLimiter for WasiState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(desired <= MAX_WASM_MEMORY_BYTES)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }
}

// ── RAII guard: deletes the output file when dropped ──────────────────────────

struct TempOutputFile(PathBuf);

impl Drop for TempOutputFile {
    fn drop(&mut self) {
        if self.0.exists() {
            if let Err(e) = std::fs::remove_file(&self.0) {
                warn!(path = %self.0.display(), error = %e, "Failed to clean up WASM output file");
            }
        }
    }
}

// ── WasmTool ──────────────────────────────────────────────────────────────────

/// An `AgentTool` backed by a pre-compiled WASM module.
pub struct WasmTool {
    engine: Arc<Engine>,
    module: Module,
    name: String,
    required_permissions: Vec<(String, PermissionOp)>,
    max_cpu_ms: u64,
    data_dir: PathBuf,
}

impl WasmTool {
    /// Pre-compile a `.wasm` file. Called once per tool at kernel boot or install time.
    pub fn load(
        engine: Arc<Engine>,
        manifest: &ToolManifest,
        wasm_path: &Path,
    ) -> Result<Self, anyhow::Error> {
        anyhow::ensure!(
            manifest.executor.executor_type == ExecutorType::Wasm,
            "WasmTool::load called on a non-WASM manifest"
        );
        let module = Module::from_file(&engine, wasm_path)?;
        let name = manifest.manifest.name.clone();
        let max_cpu_ms = manifest.sandbox.max_cpu_ms;
        let required_permissions = manifest
            .capabilities_required
            .permissions
            .iter()
            .flat_map(|p| parse_permission(p))
            .collect();

        Ok(Self {
            engine,
            module,
            name,
            required_permissions,
            max_cpu_ms,
            data_dir: PathBuf::new(), // set via with_data_dir
        })
    }

    /// Set the data directory (done by WasmToolExecutor after load).
    pub fn with_data_dir(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = data_dir;
        self
    }
}

#[async_trait]
impl AgentTool for WasmTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        self.required_permissions.clone()
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        // 1. Create a unique output file path for this invocation.
        let out_dir = self.data_dir.join("tool-out");
        std::fs::create_dir_all(&out_dir).map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: self.name.clone(),
            reason: format!("Cannot create tool-out dir: {}", e),
        })?;
        let unique_file = format!("{}-{}.json", context.task_id, uuid::Uuid::new_v4());
        let output_path = out_dir.join(&unique_file);
        // RAII guard — deletes the file when this goes out of scope.
        let _guard = TempOutputFile(output_path.clone());

        // 2. Serialize payload to JSON bytes — this becomes WASM stdin.
        let payload_bytes: Vec<u8> =
            serde_json::to_vec(&payload).map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: self.name.clone(),
                reason: format!("Payload serialization failed: {}", e),
            })?;

        // 3. Build WASI context.
        let stdin = MemoryInputPipe::new(payload_bytes);
        let stderr_pipe = MemoryOutputPipe::new(64 * 1024);
        let stderr_clone = stderr_pipe.clone();
        let output_path_str = output_path.to_string_lossy().into_owned();

        let wasi_ctx: WasiP1Ctx = WasiCtxBuilder::new()
            .stdin(stdin)
            .stderr(stderr_pipe)
            .env("AGENTOS_OUTPUT_FILE", &output_path_str)
            .build_p1();

        // 4. Set up Linker and Store.
        // Store data is WasiState which wraps WasiP1Ctx and implements ResourceLimiter
        // to enforce the MAX_WASM_MEMORY_BYTES cap on linear memory growth.
        let mut linker: Linker<WasiState> = Linker::new(&self.engine);
        wasmtime_wasi::p0::add_to_linker_async(&mut linker, |state| &mut state.ctx).map_err(
            |e| AgentOSError::ToolExecutionFailed {
                tool_name: self.name.clone(),
                reason: format!("WASI linker setup failed: {}", e),
            },
        )?;

        let mut store = Store::new(&self.engine, WasiState { ctx: wasi_ctx });
        // Enforce memory limit: prevent WASM modules from allocating beyond MAX_WASM_MEMORY_BYTES.
        store.limiter(|state| state as &mut dyn ResourceLimiter);
        // Apply CPU time limit via epoch interruption.
        store.set_epoch_deadline(1);
        let engine_clone = Arc::clone(&self.engine);
        let max_ms = self.max_cpu_ms;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(max_ms)).await;
            engine_clone.increment_epoch();
        });

        // 5. Instantiate and call _start.
        info!(
            tool = %self.name,
            task_id = %context.task_id,
            output_file = %output_path.display(),
            "Executing WASM tool"
        );

        let instance = linker
            .instantiate_async(&mut store, &self.module)
            .await
            .map_err(|e: anyhow::Error| AgentOSError::ToolExecutionFailed {
                tool_name: self.name.clone(),
                reason: format!("WASM instantiation failed: {}", e),
            })?;

        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: self.name.clone(),
                reason: format!("WASM _start not found: {}", e),
            })?;

        match start.call_async(&mut store, ()).await {
            Ok(_) => {}
            Err(e) if e.to_string().contains("epoch") => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: self.name.clone(),
                    reason: format!("Tool exceeded CPU time limit of {}ms", self.max_cpu_ms),
                });
            }
            Err(e) => {
                let stderr_content = String::from_utf8_lossy(&stderr_clone.contents()).into_owned();
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: self.name.clone(),
                    reason: format!("WASM execution failed: {}. Stderr: {}", e, stderr_content),
                });
            }
        }

        // 6. Read the output file the tool wrote.
        if !output_path.exists() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: self.name.clone(),
                reason: "Tool did not write to AGENTOS_OUTPUT_FILE".to_string(),
            });
        }

        let output_str = std::fs::read_to_string(&output_path).map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: self.name.clone(),
                reason: format!("Cannot read output file: {}", e),
            }
        })?;

        serde_json::from_str(&output_str).map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: self.name.clone(),
            reason: format!(
                "Tool output is not valid JSON: {}. Output was: {}",
                e, output_str
            ),
        })
        // _guard drops here → file deleted
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_permission(perm: &str) -> Vec<(String, PermissionOp)> {
    let parts: Vec<&str> = perm.splitn(2, ':').collect();
    if parts.len() != 2 {
        return vec![];
    }
    let resource = parts[0].to_string();
    let ops = parts[1];
    let mut result = Vec::new();
    if ops.contains('r') {
        result.push((resource.clone(), PermissionOp::Read));
    }
    if ops.contains('w') {
        result.push((resource.clone(), PermissionOp::Write));
    }
    if ops.contains('x') {
        result.push((resource.clone(), PermissionOp::Execute));
    }
    result
}
