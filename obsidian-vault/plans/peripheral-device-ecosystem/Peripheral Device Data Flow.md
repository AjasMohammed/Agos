---
title: Peripheral Device Data Flow
tags:
  - hal
  - hardware
  - peripherals
  - flow
date: 2026-03-26
status: planned
effort: 1h
priority: high
---

# Peripheral Device Data Flow

> End-to-end flow diagrams for agent-to-peripheral interaction through the AgentOS HAL.

---

## 1. General Device Interaction Flow

```
Agent Intent ("print this document")
    │
    ▼
┌─────────────────────────┐
│  Intent Router (kernel)  │
│  Resolves to tool call   │
└───────────┬─────────────┘
            │
            ▼
┌─────────────────────────┐
│  CapabilityToken Check   │
│  e.g. hardware.printer:x │
│  PermissionSet.check()   │
└───────────┬─────────────┘
            │ ✓ Allowed
            ▼
┌─────────────────────────┐
│  HAL.query(driver, params│
│           agent_id)      │
└───────────┬─────────────┘
            │
            ▼
┌─────────────────────────┐
│  Device Quarantine Gate  │
│  registry.check_access() │
│                         │
│  New device?            │
│  → auto-quarantine      │
│  → PermissionDenied     │
│  → operator must approve│
│                         │
│  Approved?              │
│  → proceed to driver    │
└───────────┬─────────────┘
            │ ✓ Approved
            ▼
┌─────────────────────────┐
│  Privacy Consent Check   │
│  (webcam, mic only)      │
│                         │
│  Active consent token?  │
│  → proceed              │
│  No consent?            │
│  → create escalation    │
│  → block until approved │
└───────────┬─────────────┘
            │ ✓ Consent granted
            ▼
┌─────────────────────────┐
│  HalDriver::query()     │
│  (actual device action)  │
│                         │
│  PrinterDriver → CUPS   │
│  UsbStorageDriver → D-Bus│
│  WebcamDriver → V4L2    │
│  etc.                   │
└───────────┬─────────────┘
            │
            ▼
┌─────────────────────────┐
│  Audit Log Entry         │
│  DeviceMounted /         │
│  PrintJobSubmitted /     │
│  WebcamCaptureStarted    │
│  etc.                   │
└───────────┬─────────────┘
            │
            ▼
┌─────────────────────────┐
│  Result → ContextWindow  │
│  (tagged as untrusted    │
│   if from external data) │
└─────────────────────────┘
```

## 2. USB Storage Mount Flow

```
Agent: "mount the USB drive"
    │
    ▼
UsbStorageDriver::query({ action: "mount", device: "sdb1" })
    │
    ▼
┌─────────────────────────────────────┐
│  1. zbus connect to system D-Bus     │
│  2. Create UDisks2 Filesystem proxy  │
│     for /org/freedesktop/UDisks2/    │
│     block_devices/sdb1               │
│  3. Call Mount({ "nosuid": true,     │
│       "noexec": true, "nodev": true })│
│  4. Receive mount_path               │
│     e.g. /media/agent-xyz/USB_DRIVE  │
│  5. Emit DeviceMounted audit event   │
└──────────────┬──────────────────────┘
               │
               ▼
Agent can now use file tools on mount_path
(injection scanner applied to file content)
    │
    ▼
Agent: "eject the USB drive"
    │
    ▼
UsbStorageDriver::query({ action: "unmount", device: "sdb1" })
    │
    ▼
┌─────────────────────────────────────┐
│  1. Call Filesystem.Unmount()        │
│  2. Call Drive.PowerOff()            │
│  3. Emit DeviceUnmounted audit event │
└─────────────────────────────────────┘
```

## 3. Print Job Flow

```
Agent: "print report.pdf on office-printer"
    │
    ▼
PrinterDriver::query({ action: "print", printer: "office-printer",
                        document_path: "/tmp/report.pdf",
                        format: "application/pdf" })
    │
    ▼
┌──────────────────────────────────────┐
│  1. Rate limit check                  │
│     (max N jobs/agent/hour)           │
│  2. ipp::AsyncIppClient connect to    │
│     ipp://localhost:631/printers/     │
│     office-printer                    │
│  3. Get-Printer-Attributes            │
│     → verify printer exists & ready   │
│  4. Create-Job                        │
│     → receive job-id                  │
│  5. Send-Document (stream PDF)        │
│  6. Get-Job-Attributes (poll status)  │
│  7. Emit PrintJobSubmitted audit event│
│  8. Return { job_id, status }         │
└──────────────────────────────────────┘
```

## 4. Privacy-Gated Capture Flow (Webcam/Mic)

```
Agent: "take a photo"
    │
    ▼
WebcamDriver::query({ action: "capture", device: "/dev/video0" })
    │
    ▼
┌──────────────────────────────────────┐
│  Privacy Consent Check                │
│                                      │
│  ConsentStore.check(agent_id,        │
│    "webcam", "/dev/video0")          │
│                                      │
│  No active consent?                  │
│  ┌──────────────────────────────┐    │
│  │ Create PendingEscalation     │    │
│  │   kind: "device_consent"     │    │
│  │   resource: "webcam:video0"  │    │
│  │   expires_at: now + 5min     │    │
│  │   context: "Agent X wants    │    │
│  │     webcam access for 60s"   │    │
│  └──────────────┬───────────────┘    │
│                 │                    │
│  Operator: agentctl escalation       │
│    resolve <id> --approve            │
│                 │                    │
│  ┌──────────────▼───────────────┐    │
│  │ ConsentStore.grant(agent_id, │    │
│  │   "webcam", ttl=60s)         │    │
│  └──────────────────────────────┘    │
└──────────────────┬───────────────────┘
                   │ ✓ Consent active
                   ▼
┌──────────────────────────────────────┐
│  V4L2 Capture                        │
│  1. Open /dev/video0                 │
│  2. Set format (640x480, MJPEG)      │
│  3. Allocate mmap buffers            │
│  4. STREAMON → DQBUF (one frame)     │
│  5. STREAMOFF                        │
│  6. Save frame to temp file          │
│  7. Emit WebcamCaptureStarted +      │
│     WebcamCaptureStopped audit events│
│  8. Return { image_path, resolution }│
└──────────────────────────────────────┘
```

## 5. Bluetooth Pairing Flow

```
Agent: "find nearby Bluetooth devices"
    │
    ▼
BluetoothDriver::query({ action: "scan", duration_seconds: 10 })
    │
    ▼
┌──────────────────────────────────────┐
│  1. bluer::Session::new()             │
│  2. Get default adapter               │
│  3. adapter.start_discovery()         │
│  4. Collect DeviceAdded events for    │
│     10 seconds                        │
│  5. adapter.stop_discovery()          │
│  6. Emit BluetoothScanStarted audit   │
│  7. Return { devices: [...] }         │
└──────────────────────────────────────┘
    │
    ▼
Agent: "pair with device XX:XX:XX:XX:XX:XX"
    │
    ▼
BluetoothDriver::query({ action: "pair", address: "XX:..." })
    │
    ▼
┌──────────────────────────────────────┐
│  Pairing requires escalation:        │
│  1. Create PendingEscalation         │
│     kind: "bluetooth_pair"           │
│     context: "Pair with 'Speaker X'" │
│  2. Operator approves + confirms     │
│     passkey if needed                │
│  3. device.pair().await              │
│  4. device.connect().await           │
│  5. Emit BluetoothPairRequested      │
│  6. Return { paired: true }          │
└──────────────────────────────────────┘
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Ecosystem Research]] — protocol details
