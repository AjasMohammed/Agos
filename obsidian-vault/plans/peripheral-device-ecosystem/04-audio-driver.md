---
title: "Phase 4: Audio Driver (PipeWire)"
tags:
  - hal
  - hardware
  - audio
  - privacy
  - phase-4
date: 2026-03-26
status: planned
effort: 2d
priority: high
---

# Phase 4: Audio Driver (PipeWire)

> Add audio capture (microphone) and playback (speakers) via PipeWire, with mandatory consent gating for microphone access.

---

## Why This Phase

Audio I/O enables voice agents, audio transcription, alert sounds, and multimedia workflows. PipeWire is the modern Linux audio server replacing PulseAudio, running per-user with no root required. Microphone capture is **privacy-critical** and requires the same consent model as the webcam driver.

## Current State → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Audio detection | Not available | `AudioDriver` enumerates PipeWire nodes |
| Microphone capture | Not available | Stream capture via PipeWire with mandatory consent |
| Audio playback | Not available | Stream playback via PipeWire (lower risk, no consent needed) |
| Privacy gate | N/A | Mandatory consent for capture; `audio.capture:x` permission |
| Feature flag | N/A | `audio` feature in `agentos-hal/Cargo.toml` |

## Detailed Subtasks

### 1. Add `pipewire` dependency (feature-gated)

**File:** `crates/agentos-hal/Cargo.toml`

```toml
[dependencies]
pipewire = { version = "0.9", optional = true }

[features]
audio = ["dep:pipewire"]
```

Note: `pipewire` crate requires `libpipewire-dev` system package. Document this in build prerequisites.

### 2. Create `AudioDriver` module

**File:** `crates/agentos-hal/src/drivers/audio.rs` (new file)

Actions:
- `list` — enumerate audio sources (microphones) and sinks (speakers)
- `capture` — record audio for N seconds from a source (requires consent)
- `playback` — play an audio file to a sink
- `volume` — get/set volume for a node

### 3. Implement PipeWire capture

PipeWire uses a callback-driven C API. The Rust `pipewire` crate mirrors this. Key challenge: PipeWire runs its own event loop — we need to bridge it to tokio.

```rust
async fn capture_audio(&self, params: &Value) -> Result<Value, AgentOSError> {
    let duration_secs = params.get("duration_seconds")
        .and_then(|v| v.as_u64()).unwrap_or(5);
    let sample_rate = params.get("sample_rate")
        .and_then(|v| v.as_u64()).unwrap_or(44100) as u32;

    // Consent check (reuse ConsentStore from Phase 3)
    // If no consent, return ConsentRequired error

    let output_path = format!("/tmp/agentos-audio-{}.wav", uuid::Uuid::new_v4());
    let output_path_clone = output_path.clone();

    // PipeWire is blocking — must run on dedicated thread
    tokio::task::spawn_blocking(move || {
        pipewire::init();
        let mainloop = pipewire::main_loop::MainLoop::new(None)
            .map_err(|e| AgentOSError::HalError(format!("PipeWire init: {e}")))?;
        let context = pipewire::context::Context::new(&mainloop)?;
        let core = context.connect(None)?;

        // Create capture stream
        let stream = pipewire::stream::Stream::new(
            &core,
            "agentos-capture",
            pipewire::properties! {
                "media.type" => "Audio",
                "media.category" => "Capture",
                "media.role" => "Communication",
            },
        )?;

        // Collect samples into buffer, write WAV when duration elapsed
        // ... (stream process callback fills buffer)

        mainloop.run();
        // Write WAV file from collected samples
        Ok::<Value, AgentOSError>(json!({
            "captured": true,
            "audio_path": output_path_clone,
            "duration_seconds": duration_secs,
            "sample_rate": sample_rate,
            "format": "wav",
        }))
    }).await
    .map_err(|e| AgentOSError::HalError(format!("Spawn blocking: {e}")))?
}
```

### 4. Implement device enumeration

Use PipeWire's registry to list nodes of type `Audio/Source` (microphones) and `Audio/Sink` (speakers):

```rust
async fn list_devices(&self) -> Result<Value, AgentOSError> {
    // Use pw-cli or pipewire registry API to enumerate
    // Return: name, media_class (Source/Sink), node_id, description
}
```

### 5. Permission model

- `hardware.audio.capture:x` — microphone (requires consent)
- `hardware.audio.playback:x` — speakers (no consent, but permission needed)
- `hardware.audio.volume:w` — volume control

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/Cargo.toml` | Add `pipewire` optional dep + `audio` feature |
| `crates/agentos-hal/src/drivers/audio.rs` | **New** — `AudioDriver` with consent-gated capture |
| `crates/agentos-hal/src/drivers/mod.rs` | Conditionally include `audio` module |
| `crates/agentos-hal/src/hal.rs` | Register driver in `new_with_defaults()` |
| `crates/agentos-types/src/lib.rs` | Add `AudioCaptureStarted`, `AudioCaptureStopped`, `AudioPlaybackStarted` events |

## Dependencies

- **Requires:** `ConsentStore` from Phase 3 (or implement independently — both phases can share)
- **Blocks:** Phase 8 (tool manifests)
- **System dep:** `libpipewire-dev` must be installed

## Test Plan

1. **Unit test — consent enforcement:** Verify capture returns `ConsentRequired` without consent.
2. **Unit test — device enumeration:** List audio nodes (may be empty in CI).
3. **Integration test (requires PipeWire):** Record 1s of silence, verify WAV file created with correct header.
4. **Playback test:** Play a short tone, verify stream completes without error.
5. **Feature flag test:** Build without `audio` feature — module excluded.

## Verification

```bash
# Ensure system dep exists
pkg-config --cflags libpipewire-0.3

cargo build -p agentos-hal --features audio
cargo test -p agentos-hal --features audio
cargo clippy -p agentos-hal --features audio -- -D warnings
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Ecosystem Research]] — PipeWire details (section 2.5)
- [[03-webcam-driver]] — shares `ConsentStore` implementation
