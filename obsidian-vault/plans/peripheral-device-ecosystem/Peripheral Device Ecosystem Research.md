---
title: Peripheral Device Ecosystem Research
tags:
  - hal
  - hardware
  - peripherals
  - research
date: 2026-03-26
status: complete
effort: 1d
priority: high
---

# Peripheral Device Ecosystem Research

> Research synthesis: Linux peripheral APIs, Rust crates, security models, and how agent frameworks handle device access.

---

## 1. How Agent Frameworks Handle Hardware Today

### Findings (NotebookLM, sourced from AI Agent Frameworks & AgentOS 2025-2026)

**No framework has native peripheral abstractions.** LangChain, AutoGPT, CrewAI, and OpenInterpreter all treat hardware access as external "tools" — the developer writes a custom integration that shells out or calls an API.

- **CrewAI** — agents connect to "any local tool" via role-based orchestration. No built-in device primitives.
- **LangChain** — composable primitives; hardware access via custom "Agents" or "Custom Chains" wrapping third-party integrations.
- **AutoGPT** — browser-native, typically runs in Docker. Host peripheral access is friction-heavy.
- **MCP (Model Context Protocol)** — the strongest signal. Acts as a "universal USB-C port" for AI applications. JSON-RPC 2.0 transport. MCP servers can expose device-control tools; agents import them dynamically. **This is the ecosystem bridge strategy.**

**Key insight:** AgentOS is unique in treating hardware as a kernel-level resource rather than an afterthought. The HAL + quarantine + capability token model has no equivalent in other frameworks.

### Native HAL vs MCP-Bridged Access (tradeoffs)

| Dimension | Native HAL Driver | MCP-Bridged |
|-----------|------------------|-------------|
| Latency | Lowest — direct syscall/ioctl | Higher — JSON-RPC + potential network hop |
| Security | Deep — HMAC tokens + seccomp + device quarantine | Standardized — relies on MCP server trust + user consent |
| Capability | Full system access at OS level | Limited to what the MCP server exposes |
| Dev effort | High — write Rust driver per device | Low — use existing community MCP servers |
| Interop | AgentOS only | Works across LangGraph, CrewAI, AutoGen, etc. |

**Recommendation:** Native HAL for the 7 core peripherals (this plan). MCP bridge for niche/third-party devices.

---

## 2. Linux Peripheral Protocols and Rust Crates

### 2.1 CUPS/IPP — Printing

- **Protocol:** IPP (Internet Printing Protocol, RFC 8010/8011). HTTP-based, POST to `ipp://host:631/printers/name`.
- **Flow:** `Get-Printer-Attributes` → `Create-Job` → `Send-Document` → `Get-Job-Attributes` (poll status)
- **Rust crate:** `ipp` v5.4.0 — implements RFC 8010/8011, sync + async. `IppOperationBuilder` + `AsyncIppClient`.
- **Auth:** No root for job submission. `lpadmin` group for printer admin. CUPS has its own ACL in `cupsd.conf`.
- **Risks:** Print queue flooding (DoS), paper/ink waste, network CUPS CVEs (filter chain buffer overflows).
- **Mitigation:** Rate-limit jobs, require user approval, validate document format.

### 2.2 UDisks2 — USB Storage Mount/Unmount

- **Protocol:** D-Bus on system bus at `org.freedesktop.UDisks2`. Objects under `/org/freedesktop/UDisks2/block_devices/` and `/drives/`.
- **Flow:** `GetManagedObjects()` → monitor `InterfacesAdded` → `Filesystem.Mount(options)` → returns mount path → `Filesystem.Unmount()` → `Drive.PowerOff()`
- **Rust crate:** `zbus` v5.14.0 — build type-safe proxies from UDisks2 introspection XML. No dedicated UDisks2 crate.
- **Auth:** Polkit-gated. Removable devices at active seat: auto-authorized. System/internal devices: admin auth required.
- **Risks:** Kernel filesystem driver vulns from untrusted FS, malicious `.desktop` files, symlink attacks.
- **Mitigation:** Mount with `nosuid,noexec,nodev`. Treat all mounted content as untrusted. User approval per mount.

### 2.3 V4L2 — Webcam/Video Capture

- **Protocol:** Kernel ioctl API on `/dev/videoN`. Negotiate format → allocate mmap buffers → STREAMON → DQBUF loop.
- **Flow:** Open device → `VIDIOC_QUERYCAP` → `VIDIOC_S_FMT` → `VIDIOC_REQBUFS` + `mmap()` → `VIDIOC_STREAMON` → frame loop → `VIDIOC_STREAMOFF`
- **Rust crate:** `v4l` (libv4l-rs) — safe bindings with `CaptureDevice` + `MmapStream`. Also `nokhwa` for cross-platform.
- **Auth:** `video` group membership. No polkit, no root.
- **Risks:** **Critical privacy** — silent webcam recording. No kernel-level permission prompt (unlike mobile).
- **Mitigation:** Mandatory escalation consent per capture session. Time-limited access tokens.

### 2.4 BlueZ — Bluetooth

- **Protocol:** D-Bus on system bus. Adapters at `/org/bluez/hci0`, devices at `/org/bluez/hci0/dev_XX_XX_XX_XX_XX_XX`.
- **Flow:** `GetManagedObjects()` → `Adapter1.StartDiscovery()` → monitor `InterfacesAdded` → `Device1.Pair()` → `Device1.Connect()` → BLE GATT `ReadValue()/WriteValue()`
- **Rust crate:** `bluer` (official BlueZ bindings) — adapters, GATT client/server, L2CAP, RFCOMM. Or `bluebus` (built on `zbus`).
- **Auth:** `bluetooth` group + D-Bus policy in `/etc/dbus-1/system.d/bluetooth.conf`.
- **Risks:** Rogue device pairing, Bluetooth tracking (MAC exposure), BlueBorne kernel vulns.
- **Mitigation:** User approval for discovery + pairing. Time-limited scans. Agent pairing requires escalation.

### 2.5 PipeWire — Audio

- **Protocol:** Unix socket at `/run/user/$UID/pipewire-0`. Stream API: create capture (INPUT) or playback (OUTPUT) stream.
- **Flow:** Connect to daemon → enumerate devices via registry → create `pw_stream` → negotiate format → streaming callback → disconnect.
- **Rust crate:** `pipewire` v0.9.2 (official bindings, requires `libpipewire-dev`). Also `cpal` (RustAudio) for cross-platform abstraction.
- **Auth:** Per-user Unix socket. No root, no polkit for native apps. Sandboxed apps go through XDG Desktop Portal.
- **Risks:** **Critical privacy** — silent microphone recording.
- **Mitigation:** Mandatory escalation consent for mic capture. Playback is lower risk but still requires `audio.playback:x` permission.

### 2.6 Wayland Output Management — Display Config

- **Protocol:** `wlr-output-management-unstable-v1` (wlroots compositors). GNOME uses `org.gnome.Mutter.DisplayConfig` D-Bus.
- **Flow:** Bind `zwlr_output_manager_v1` → receive `head` events → `create_configuration(serial)` → `enable_head` + `set_mode/position/scale` → `apply()` or `test()`.
- **Rust crate:** `wayland-protocols-wlr` (Smithay) + `wayland-client`. For GNOME: `zbus` to call Mutter D-Bus.
- **Auth:** Compositor security policy. No polkit.
- **Risks:** Bad config can brick display (DoS). Disabling outputs locks user out.
- **Mitigation:** Always `test()` before `apply()`. Auto-revert after 15s if not confirmed.

### 2.7 libusb — Raw USB

- **Protocol:** User-space USB via `/dev/bus/usb/BBB/DDD` (usbfs). Open device → claim interface → bulk/interrupt/control transfers.
- **Flow:** Enumerate → filter by vendor/product → `open()` → `claim_interface()` → `read_bulk()/write_bulk()` → release.
- **Rust crate:** `nusb` v0.2.2 (pure Rust, async, no C FFI) — recommended. Or `rusb` v0.9.4 (wraps libusb).
- **Auth:** Root by default. Users gain access via udev rules matching vendor/product ID.
- **Risks:** **Critical** — unrestricted hardware access. Detaching kernel drivers can disrupt system devices. BadUSB attacks.
- **Mitigation:** Most restricted permission tier. Per-device vendor/product whitelist. Block `detach_kernel_driver` by default. Explicit approval only.

---

## 3. Security Model Summary

### Permission Tiers for Peripherals

| Tier | Devices | Gate |
|------|---------|------|
| **Low risk** | Printer (output only), Display (config) | User approval + rate limit / auto-revert |
| **Medium risk** | USB storage, Bluetooth | User approval per operation + content treated as untrusted |
| **High risk (privacy)** | Webcam, Microphone | Mandatory escalation consent per session + time-limited |
| **Critical (raw access)** | Raw USB | Per-device vendor/product whitelist + explicit approval + no kernel driver detach |

### Audit Events (new)

| Event | Fields |
|-------|--------|
| `DeviceMounted` | agent_id, device_key, mount_path, mount_options |
| `DeviceUnmounted` | agent_id, device_key |
| `PrintJobSubmitted` | agent_id, printer_name, document_format, page_count |
| `WebcamCaptureStarted` | agent_id, device_path, resolution, consent_id |
| `WebcamCaptureStopped` | agent_id, device_path, frames_captured |
| `AudioCaptureStarted` | agent_id, device_name, sample_rate, consent_id |
| `AudioCaptureStopped` | agent_id, device_name, duration_ms |
| `BluetoothScanStarted` | agent_id, adapter, duration_limit |
| `BluetoothPairRequested` | agent_id, device_address, device_name |
| `DisplayConfigApplied` | agent_id, output_name, resolution, revert_timeout |
| `RawUsbTransfer` | agent_id, vendor_id, product_id, endpoint, direction, bytes |

---

## 4. Dependency Plan (Cargo.toml additions)

All dependencies are feature-gated:

```toml
[dependencies]
# D-Bus (shared by usb-storage, bluetooth)
zbus = { version = "5", optional = true }

# Printing
ipp = { version = "5", optional = true }

# Webcam
v4l = { version = "0.14", optional = true }

# Audio
pipewire = { version = "0.9", optional = true }

# Bluetooth (alternative to zbus for BlueZ)
bluer = { version = "0.17", optional = true, features = ["full"] }

# Display
wayland-client = { version = "0.31", optional = true }
wayland-protocols-wlr = { version = "0.3", optional = true }

# Raw USB
nusb = { version = "0.2", optional = true }

[features]
default = []
usb-storage = ["dep:zbus"]
printer = ["dep:ipp"]
webcam = ["dep:v4l"]
audio = ["dep:pipewire"]
bluetooth = ["dep:bluer"]
display = ["dep:wayland-client", "dep:wayland-protocols-wlr"]
raw-usb = ["dep:nusb"]
all-peripherals = ["usb-storage", "printer", "webcam", "audio", "bluetooth", "display", "raw-usb"]
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Data Flow]] — flow diagrams
