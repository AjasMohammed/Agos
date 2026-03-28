---
title: "Phase 3: Webcam Driver (V4L2)"
tags:
  - hal
  - hardware
  - webcam
  - privacy
  - phase-3
date: 2026-03-26
status: planned
effort: 2d
priority: high
---

# Phase 3: Webcam Driver (V4L2)

> Add webcam discovery and frame capture via V4L2, with mandatory privacy consent gating.

---

## Why This Phase

Webcam access enables computer vision tasks, photo documentation, QR code scanning, and video monitoring. However, **camera access is a critical privacy concern** — silent recording must be impossible. This driver integrates V4L2 for capture AND the escalation manager for mandatory consent.

## Current State → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Webcam detection | Not available | `WebcamDriver` enumerates `/dev/video*` devices |
| Frame capture | Not available | Single-frame and burst capture via V4L2 mmap |
| Privacy gate | N/A | **Mandatory** escalation consent per capture session |
| Consent TTL | N/A | Configurable (default 60s), auto-expires |
| Feature flag | N/A | `webcam` feature in `agentos-hal/Cargo.toml` |

## Detailed Subtasks

### 1. Add `v4l` dependency (feature-gated)

**File:** `crates/agentos-hal/Cargo.toml`

```toml
[dependencies]
v4l = { version = "0.14", optional = true }

[features]
webcam = ["dep:v4l"]
```

### 2. Create `WebcamDriver` module

**File:** `crates/agentos-hal/src/drivers/webcam.rs` (new file)

Actions:
- `list` — enumerate video devices with capabilities
- `capture` — capture a single frame (requires consent)
- `burst` — capture N frames at interval (requires consent)

### 3. Implement consent checking

The driver must check for an active consent token before any capture. The consent store is a simple in-memory map:

```rust
pub struct ConsentStore {
    /// (agent_id, resource) → expires_at
    grants: RwLock<HashMap<(String, String), Instant>>,
}

impl ConsentStore {
    pub fn check(&self, agent_id: &str, resource: &str) -> bool {
        self.grants.read().unwrap()
            .get(&(agent_id.to_string(), resource.to_string()))
            .map(|exp| Instant::now() < *exp)
            .unwrap_or(false)
    }

    pub fn grant(&self, agent_id: &str, resource: &str, ttl: Duration) {
        self.grants.write().unwrap()
            .insert((agent_id.to_string(), resource.to_string()), Instant::now() + ttl);
    }
}
```

If no consent exists, the driver returns a special error that the kernel translates into a `PendingEscalation`. The operator approves via `agentctl escalation resolve`.

### 4. Implement V4L2 capture

```rust
async fn capture_frame(&self, params: &Value) -> Result<Value, AgentOSError> {
    let device_path = params.get("device").and_then(|v| v.as_str())
        .unwrap_or("/dev/video0");

    // V4L2 operations are blocking — spawn on blocking thread
    let device_path = device_path.to_string();
    let result = tokio::task::spawn_blocking(move || {
        use v4l::prelude::*;
        use v4l::video::Capture;

        let dev = CaptureDevice::new(0)
            .map_err(|e| AgentOSError::HalError(format!("Open webcam: {e}")))?;

        // Set format
        let mut fmt = dev.format()
            .map_err(|e| AgentOSError::HalError(format!("Get format: {e}")))?;
        fmt.width = 640;
        fmt.height = 480;
        dev.set_format(&fmt)
            .map_err(|e| AgentOSError::HalError(format!("Set format: {e}")))?;

        // Create mmap stream and grab one frame
        let mut stream = MmapStream::with_buffers(&dev, v4l::buffer::Type::VideoCapture, 4)
            .map_err(|e| AgentOSError::HalError(format!("Create stream: {e}")))?;

        let (buf, _meta) = stream.next()
            .map_err(|e| AgentOSError::HalError(format!("Capture frame: {e}")))?;

        // Save to temp file
        let tmp_path = format!("/tmp/agentos-capture-{}.jpg", uuid::Uuid::new_v4());
        std::fs::write(&tmp_path, &buf)
            .map_err(|e| AgentOSError::HalError(format!("Write frame: {e}")))?;

        Ok::<Value, AgentOSError>(json!({
            "captured": true,
            "image_path": tmp_path,
            "width": fmt.width,
            "height": fmt.height,
            "format": format!("{:?}", fmt.fourcc),
        }))
    }).await
    .map_err(|e| AgentOSError::HalError(format!("Spawn blocking: {e}")))?;

    result
}
```

### 5. Implement device enumeration

```rust
async fn list_devices(&self) -> Result<Value, AgentOSError> {
    // Enumerate /dev/video* via sysfs
    let mut devices = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/video4linux/") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let dev_name = std::fs::read_to_string(entry.path().join("name"))
                .unwrap_or_default().trim().to_string();
            devices.push(json!({
                "device": format!("/dev/{}", name),
                "name": dev_name,
            }));
        }
    }
    Ok(json!({ "devices": devices }))
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/Cargo.toml` | Add `v4l` optional dep + `webcam` feature |
| `crates/agentos-hal/src/drivers/webcam.rs` | **New** — `WebcamDriver` with consent gating |
| `crates/agentos-hal/src/drivers/mod.rs` | Conditionally include `webcam` module |
| `crates/agentos-hal/src/hal.rs` | Register driver in `new_with_defaults()` |
| `crates/agentos-hal/src/consent.rs` | **New** — `ConsentStore` (shared by webcam + audio) |
| `crates/agentos-types/src/lib.rs` | Add `WebcamCaptureStarted`, `WebcamCaptureStopped` events |

## Dependencies

- **Requires:** Escalation manager (already exists in kernel)
- **Blocks:** Phase 8 (tool manifests)

## Test Plan

1. **Unit test — consent check:** Grant consent with 1s TTL, verify access allowed then denied after expiry.
2. **Unit test — device enumeration:** Mock `/sys/class/video4linux/` (or test on real system).
3. **Unit test — no-consent capture:** Verify capture returns consent-required error.
4. **Feature flag test:** Build without `webcam` feature — module excluded.
5. **Permission test:** Verify driver requires `hardware.webcam:x` permission.

## Verification

```bash
cargo build -p agentos-hal --features webcam
cargo test -p agentos-hal --features webcam
cargo clippy -p agentos-hal --features webcam -- -D warnings
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Ecosystem Research]] — V4L2 protocol details (section 2.3)
- [[Peripheral Device Data Flow]] — privacy-gated capture flow (section 4)
