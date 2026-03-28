---
title: "Phase 2: Printer Driver (CUPS/IPP)"
tags:
  - hal
  - hardware
  - printer
  - phase-2
date: 2026-03-26
status: planned
effort: 2d
priority: high
---

# Phase 2: Printer Driver (CUPS/IPP)

> Add print job submission, printer discovery, and job status polling via the IPP protocol and CUPS.

---

## Why This Phase

Printing is a fundamental office automation capability. Agents generating reports, invoices, or documents need to send them to a printer. No current HAL driver provides printing. IPP (Internet Printing Protocol) is the standard — CUPS implements it natively on Linux/macOS.

## Current State → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Printer detection | Not available | `PrinterDriver` discovers printers via `Get-Printer-Attributes` |
| Print job submission | Not available | `PrinterDriver` submits jobs via `Create-Job` + `Send-Document` |
| Job status | Not available | Poll `Get-Job-Attributes` for status |
| Rate limiting | N/A | Max 10 jobs per agent per hour (configurable) |
| Feature flag | N/A | `printer` feature in `agentos-hal/Cargo.toml` |

## Detailed Subtasks

### 1. Add `ipp` dependency (feature-gated)

**File:** `crates/agentos-hal/Cargo.toml`

```toml
[dependencies]
ipp = { version = "5", optional = true }

[features]
printer = ["dep:ipp"]
```

### 2. Create `PrinterDriver` module

**File:** `crates/agentos-hal/src/drivers/printer.rs` (new file)

Actions:
- `list` — discover available printers (iterate known CUPS printer URIs)
- `print` — submit a print job (params: `printer`, `document_path`, `format`, `copies`)
- `status` — check job status (params: `job_id`)
- `cancel` — cancel a pending job (params: `job_id`)

```rust
pub struct PrinterDriver {
    /// Rate limiter: agent_id → (count, window_start)
    rate_limits: Arc<RwLock<HashMap<String, (u32, Instant)>>>,
    max_jobs_per_hour: u32,
}

impl PrinterDriver {
    pub fn new() -> Self {
        Self {
            rate_limits: Arc::new(RwLock::new(HashMap::new())),
            max_jobs_per_hour: 10,
        }
    }
}
```

### 3. Implement IPP operations

**Print flow:**
```rust
async fn print_document(&self, params: &Value) -> Result<Value, AgentOSError> {
    let printer_name = params.get("printer").and_then(|v| v.as_str())
        .ok_or_else(|| AgentOSError::HalError("Missing 'printer' param".into()))?;
    let doc_path = params.get("document_path").and_then(|v| v.as_str())
        .ok_or_else(|| AgentOSError::HalError("Missing 'document_path' param".into()))?;

    // Block path traversal
    if doc_path.contains("..") {
        return Err(AgentOSError::HalError("Path traversal blocked".into()));
    }

    // Rate limit check (per agent_id passed via params)
    self.check_rate_limit(params)?;

    let printer_uri = format!("ipp://localhost:631/printers/{}", printer_name);
    let client = ipp::AsyncIppClient::new(&printer_uri);

    // Read document
    let doc_bytes = tokio::fs::read(doc_path).await
        .map_err(|e| AgentOSError::HalError(format!("Read document: {e}")))?;

    let format = params.get("format").and_then(|v| v.as_str())
        .unwrap_or("application/pdf");

    // Create-Job
    let create_op = ipp::IppOperationBuilder::new()
        .create_job(&printer_uri)
        .build();
    let create_resp = client.send(create_op).await?;
    let job_id = create_resp.attributes().get("job-id")...;

    // Send-Document
    let send_op = ipp::IppOperationBuilder::new()
        .send_document(job_id, doc_bytes, format)
        .build();
    client.send(send_op).await?;

    Ok(json!({
        "submitted": true,
        "job_id": job_id,
        "printer": printer_name,
        "format": format,
    }))
}
```

### 4. Implement `HalDriver` trait

```rust
#[async_trait]
impl HalDriver for PrinterDriver {
    fn name(&self) -> &str { "printer" }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.printer", PermissionOp::Execute)
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        params.get("printer")
            .and_then(|v| v.as_str())
            .map(|p| format!("printer:{}", p))
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match params.get("action").and_then(|a| a.as_str()).unwrap_or("list") {
            "list" => self.list_printers().await,
            "print" => self.print_document(&params).await,
            "status" => self.job_status(&params).await,
            "cancel" => self.cancel_job(&params).await,
            other => Err(AgentOSError::HalError(format!("Unknown printer action: {}", other))),
        }
    }
}
```

### 5. Register driver + audit events

Same pattern as Phase 1: conditional module inclusion, `new_with_defaults()` registration, `PrintJobSubmitted` audit event.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/Cargo.toml` | Add `ipp` optional dep + `printer` feature |
| `crates/agentos-hal/src/drivers/printer.rs` | **New** — `PrinterDriver` with rate limiting |
| `crates/agentos-hal/src/drivers/mod.rs` | Conditionally include `printer` module |
| `crates/agentos-hal/src/hal.rs` | Register driver in `new_with_defaults()` |
| `crates/agentos-types/src/lib.rs` | Add `PrintJobSubmitted`, `PrintJobCancelled` events |

## Dependencies

- **Requires:** None (independent of other phases)
- **Blocks:** Phase 8 (tool manifests)

## Test Plan

1. **Unit test — rate limiting:** Submit 11 mock jobs in rapid succession, verify 11th is rejected.
2. **Unit test — path traversal:** Verify `document_path: "../etc/passwd"` is rejected.
3. **Unit test — device key:** Verify `device_key` returns `"printer:office-hp"` for printer name `"office-hp"`.
4. **Integration test (requires CUPS):** If CUPS is running, list printers, submit a test job to PDF virtual printer.
5. **Feature flag test:** Build without `printer` feature — module excluded.

## Verification

```bash
cargo build -p agentos-hal --features printer
cargo test -p agentos-hal --features printer
cargo clippy -p agentos-hal --features printer -- -D warnings
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Ecosystem Research]] — CUPS/IPP protocol details (section 2.1)
- [[Peripheral Device Data Flow]] — print flow diagram (section 3)
