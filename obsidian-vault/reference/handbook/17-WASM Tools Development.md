---
title: WASM Tools Development
tags:
  - tools
  - wasm
  - sdk
  - handbook
  - v3
date: 2026-03-17
status: complete
effort: 3h
priority: high
---

# WASM Tools Development

> Custom tools extend AgentOS without modifying the kernel. They run as WebAssembly modules under Wasmtime isolation, or as native Rust code using the `#[tool]` SDK macro — each approach has different tradeoffs in portability, isolation, and development complexity.

---

## WASM Tool Protocol

When the kernel runs a WASM tool it follows this sequence:

```
1. Generate unique output path: {data_dir}/tool-out/{task_id}-{uuid}.json
2. Serialize input payload → JSON bytes → WASM stdin
3. Set AGENTOS_OUTPUT_FILE env var → unique output path
4. Wasmtime instantiates the module and calls _start
5. Module writes JSON result to $AGENTOS_OUTPUT_FILE
6. Kernel reads JSON from that file, then deletes it (RAII guard)
```

**Exit codes:** The kernel does not use the exit code as a success signal. The presence of valid JSON at `AGENTOS_OUTPUT_FILE` is what determines success. A non-zero exit or absent output file is treated as an error. **Stderr** is captured and included in the error message when the module fails.

**Your module must:**
- Read its JSON input payload from **stdin**
- Compute a result
- Write a valid JSON object to the file path stored in `AGENTOS_OUTPUT_FILE`

**Your module must not:**
- Write result data to stdout (it is ignored)
- Assume filesystem access (no preopened directories are granted)
- Assume network access (no socket capability unless `sandbox.network = true` in the manifest)

---

## Rust WASM Tool

Target: `wasm32-wasip1` (WASI preview 1).

### Install the target

```bash
rustup target add wasm32-wasip1
```

### `Cargo.toml`

```toml
[package]
name = "hello-tool"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "hello-tool"
path = "src/main.rs"

[dependencies]
serde_json = "1"
```

### `src/main.rs`

```rust
use std::env;
use std::io::{self, Read};

fn main() {
    // 1. Read the JSON payload from stdin.
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).expect("read stdin");

    let payload: serde_json::Value =
        serde_json::from_str(&input).expect("parse JSON payload");

    // 2. Extract input fields.
    let query = payload["query"].as_str().unwrap_or("").to_lowercase();

    // 3. Do the work.
    let result = serde_json::json!({
        "matched": query.contains("hello"),
        "echo": query,
    });

    // 4. Write JSON result to AGENTOS_OUTPUT_FILE.
    let out_path = env::var("AGENTOS_OUTPUT_FILE")
        .expect("AGENTOS_OUTPUT_FILE must be set by the kernel");
    std::fs::write(&out_path, serde_json::to_string(&result).unwrap())
        .expect("write output file");
}
```

### Build

```bash
cargo build --target wasm32-wasip1 --release
# Binary: target/wasm32-wasip1/release/hello-tool.wasm
```

---

## Python WASM Tool

> **Status:** Python-to-WASM compilation via py2wasm is planned but not yet implemented. Currently, only Rust WASM tools targeting `wasm32-wasip1` are supported.

Python tools can be compiled to WASM with **py2wasm** (Nuitka-based) or run inside a WASM Python interpreter.

### Install py2wasm

```bash
pip install py2wasm
```

### `hello_tool.py`

```python
import json
import os
import sys

def main():
    # 1. Read JSON payload from stdin.
    payload = json.loads(sys.stdin.read())

    # 2. Extract input fields.
    query = payload.get("query", "").lower()

    # 3. Do the work.
    result = {
        "matched": "hello" in query,
        "echo": query,
    }

    # 4. Write JSON result to AGENTOS_OUTPUT_FILE.
    out_path = os.environ["AGENTOS_OUTPUT_FILE"]
    with open(out_path, "w") as f:
        json.dump(result, f)

if __name__ == "__main__":
    main()
```

### Compile to WASM

```bash
py2wasm hello_tool.py -o hello-tool.wasm
```

The resulting `hello-tool.wasm` is used exactly the same way as the Rust-compiled module — place it next to your manifest and set `wasm_path` accordingly.

---

## Tool Manifest for WASM

WASM tools use all the standard manifest fields plus an `[executor]` section pointing to the compiled module.

```toml
[manifest]
name        = "hello-tool"
version     = "0.1.0"
description = "Returns a greeting match based on the input query"
author      = "alice@example.com"
trust_tier  = "community"
author_pubkey = "<64 hex chars>"    # set by `agentctl tool sign`
signature     = "<128 hex chars>"  # set by `agentctl tool sign`

[capabilities_required]
permissions = []                    # no special permissions needed for this example

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "HelloInput"
output = "HelloOutput"

[sandbox]
network       = false
fs_write      = false
gpu           = false
max_memory_mb = 64     # Wasmtime ResourceLimiter enforces this cap
max_cpu_ms    = 5000   # epoch interruption fires after this many milliseconds

[executor]
type      = "wasm"
wasm_path = "hello-tool.wasm"   # relative to this manifest's directory
```

The `.wasm` binary and the manifest must be in the same directory (or adjust `wasm_path` to a relative path within the directory). When `agentctl tool install` is called, the kernel resolves `wasm_path` relative to the manifest's location, pre-compiles the module with Wasmtime, and registers it.

---

## Wasmtime Sandbox Guarantees

The WASM runtime (`crates/agentos-wasm`) provides these isolation guarantees for every module invocation:

### Capability isolation

WASI capabilities are constructed from scratch for each invocation using `WasiCtxBuilder`. The module receives:

- **stdin** — the serialized JSON payload bytes
- **stderr** — captured pipe for error/log output (included in error messages)
- **`AGENTOS_OUTPUT_FILE`** — environment variable pointing to the unique output path

The module does **not** receive:
- Preopened host filesystem directories
- Network socket capability (unless `sandbox.network = true`)
- Any other environment variables from the host

### Memory limits

`WasiState` implements Wasmtime's `ResourceLimiter` trait. When the WASM module attempts to grow its linear memory past **256 MiB**, `memory_growing()` returns `false` — the growth is denied and the module typically traps with an out-of-memory error.

### Epoch interruption (CPU time limit)

Before calling `_start`, the kernel executes:
```rust
store.set_epoch_deadline(1);
```
A background Tokio task sleeps for `max_cpu_ms` milliseconds, then calls:
```rust
engine.increment_epoch();
```
If the module is still executing, Wasmtime interrupts it. The kernel catches the trap (which contains "epoch" in the message) and returns a timeout error to the calling agent.

### Output file cleanup

A `TempOutputFile` RAII guard wraps the output path. When the guard is dropped — after the result is read, or on any error path — it deletes the file. Output files from timed-out or crashed modules are always cleaned up.

---

## SDK `#[tool]` Macro

For native Rust tools that compile directly into the kernel (not WASM modules), the `agentos-sdk` crate provides the `#[tool]` proc macro. This is the simplest way to write a built-in tool.

### `Cargo.toml`

```toml
[dependencies]
agentos-sdk = { path = "../agentos-sdk" }
agentos-tools = { path = "../agentos-tools" }
agentos-types = { path = "../agentos-types" }
serde_json = "1"
async-trait = "0.1"
```

### Usage

```rust
use agentos_sdk::tool;
use agentos_tools::traits::ToolExecutionContext;
use agentos_types::AgentOSError;

#[tool(
    name = "word-count",
    version = "1.0.0",
    description = "Count the number of words in a text string",
    permissions = "fs.user_data:r"
)]
async fn word_count(
    payload: serde_json::Value,
    _context: ToolExecutionContext,
) -> Result<serde_json::Value, AgentOSError> {
    let text = payload["text"].as_str().unwrap_or("");
    let count = text.split_whitespace().count();
    Ok(serde_json::json!({ "word_count": count }))
}
```

**Permission syntax:** `"resource:ops"` where ops are `r` (Read), `w` (Write), `x` (Execute), or compounds `rw`, `rx`, `wx`, `rwx`. Unknown op suffixes produce a compile error.

**What the macro generates:**

```rust
pub struct WordCount;

#[async_trait]
impl AgentTool for WordCount {
    fn name(&self) -> &str { "word-count" }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        word_count(payload, context).await
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Read)]
    }
}

impl WordCount {
    pub fn version() -> &'static str { "1.0.0" }
    pub fn description() -> &'static str { "Count the number of words..." }
}
```

**Register with `ToolRunner`:**

```rust
runner.register(Box::new(WordCount));
```

> **Note:** The `#[tool]` macro is for native Rust tools compiled into the kernel binary. It does not produce a WASM module. WASM tools use the stdin/stdout protocol described above and need a separate `[executor]` section in their manifest.

---

## Testing Tools Locally

### Testing a WASM binary standalone

Simulate the kernel's execution protocol using the `wasmtime` CLI:

```bash
# Build
cargo build --target wasm32-wasip1 --release

# Create a test payload
echo '{"query": "hello world"}' > /tmp/input.json

# Run exactly as the kernel would
AGENTOS_OUTPUT_FILE=/tmp/output.json \
  wasmtime run \
    --env AGENTOS_OUTPUT_FILE=/tmp/output.json \
    target/wasm32-wasip1/release/hello-tool.wasm \
    < /tmp/input.json

# Inspect the result
cat /tmp/output.json
# {"matched":true,"echo":"hello world"}
```

### Testing via the kernel

```bash
# 1. Start the kernel
agentctl serve

# 2. Sign and install your tool
agentctl tool sign --manifest my-tool.toml --key my-keypair.json
agentctl tool install my-tool.toml

# 3. Confirm it appears
agentctl tool list

# 4. Run an agent that exercises the tool
agentctl task run --agent my-agent --goal "Use hello-tool to check 'hello world'"
```

### Unit testing the protocol (native binary)

```bash
# For a native binary (useful in CI before cross-compiling):
echo '{"query": "test"}' | AGENTOS_OUTPUT_FILE=/tmp/out.json ./target/debug/hello-tool
cat /tmp/out.json
```

For Python tools:

```bash
echo '{"query": "test"}' | AGENTOS_OUTPUT_FILE=/tmp/out.json python hello_tool.py
cat /tmp/out.json
```

---

## Publishing Workflow

```bash
# 1. Generate a keypair (one-time)
agentctl tool keygen --output my-keypair.json
# Store my-keypair.json securely — never commit to version control.

# 2. Build the WASM binary
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/hello-tool.wasm ./hello-tool.wasm

# 3. Create the manifest (set trust_tier = "community")
# ... edit hello-tool.toml ...

# 4. Sign the manifest
agentctl tool sign --manifest hello-tool.toml --key my-keypair.json

# 5. Verify the signature
agentctl tool verify hello-tool.toml
# OK  hello-tool (trust_tier=community)

# 6. Distribute: share hello-tool.toml and hello-tool.wasm together
# Users install with:
agentctl tool install hello-tool.toml

# 7. If you need to revoke a release:
#    Ask the AgentOS project to add your pubkey to the CRL.
#    Kernels with the updated CRL will reject all tools signed with that key.
```

The manifest and `.wasm` binary must be distributed together in the same directory (or adjust `wasm_path`). The kernel verifies the signature during `install` — installation fails if the manifest has been tampered with after signing. A tampered `version`, `name`, `author`, or sandbox field invalidates the signature.

---

## Related

- [[07-Tool System]] — Tool manifests, trust tiers, signing, built-in tools reference
- [[Architecture Overview]] — How the kernel dispatches tool execution
- [[Security Reference]] — Capability tokens and PermissionSet
