---
title: Advanced Operations
tags:
  - reference
  - advanced
  - operations
  - v3
date: 2026-03-17
status: complete
---

# Advanced Operations

> Reference for advanced AgentOS subsystems: Hardware Abstraction Layer (HAL), resource arbitration, context snapshots and rollback, escalation management, and agent identity.

---

## Hardware Abstraction Layer (HAL)

The HAL (`crates/agentos-hal`) provides a device registry that controls which agents can access hardware resources. All newly detected devices start in a quarantined state and must be explicitly approved before any agent can use them.

### Device Lifecycle

```
Detected → Pending → Approved    (approved for specific agents)
                   → Quarantined (hard-denied for all agents)
```

State transitions:

| Transition | Operation |
|---|---|
| Any → Pending | Device detected for the first time (awaiting approval) |
| Pending → Approved | Administrator approves the device for one or more agents |
| Pending → Quarantined | Administrator explicitly blocks the device |
| Approved → Quarantined | Last approved agent's access is revoked, or administrator blocks the device |
| Quarantined → _(no transition)_ | Quarantined devices cannot be re-approved. The `register_device` method is idempotent and does not reset existing state, so there is currently no code path to move a device out of `Quarantined`. |

> **Per-agent denial:** The implementation also supports `deny_for_agent(device_id, agent_id)`, which adds granular per-agent denial on an otherwise `Approved` device. Denied agent IDs are tracked in a `denied_to` set on the device entry. This allows blocking a specific agent from a device without quarantining it for all agents.

### Device ID Format

Devices are identified by strings like `gpu:0`, `usb:1`, `cam:0`, `mic:0`. The identifier scheme is `<type>:<index>`.

### HAL CLI

The HAL CLI is available via `agentos hal`:

```bash
# List all registered devices
agentos hal list

# Show devices currently in quarantine
agentos hal quarantine

# Approve a device for a specific agent
agentos hal approve --device gpu:0 --agent coder

# Deny a device for all agents
agentos hal deny --device usb:1

# Revoke an agent's access to a device
agentos hal revoke --device cam:0 --agent researcher
```

### Audit Events

Device lifecycle changes are recorded in the audit log:

| Event | Trigger |
|---|---|
| `HardwareDeviceDetected` | Device first seen by the registry |
| `HardwareDeviceApproved` | Device approved for an agent |
| `HardwareDeviceDenied` | Device denied |
| `HardwareDeviceRevoked` | Agent's access revoked |

### HAL Drivers

The HAL ships **16 drivers** — some always available, others feature-gated at compile time. IoT drivers (`mqtt`, `homeassistant`) integrate with the HAL device-twin model: an agent sets a desired state, the safety engine validates the request, and an actuator update is dispatched.

| Driver | Feature Gate | Description |
|--------|-------------|-------------|
| `system` | — (always) | System info: hostname, uptime, OS, CPU, memory |
| `process` | — (always) | Process listing, signals, resource usage |
| `network` | — (always) | Network interfaces, connections, bandwidth |
| `storage` | — (always) | Block device listing, disk usage |
| `sensor` | — (always) | Thermal sensor readings |
| `gpu` | — (always) | GPU metrics (VRAM, temperature, utilization) |
| `log_reader` | — (always) | System and application log reading |
| `audio` | `audio` | Audio capture and playback via PipeWire/PulseAudio |
| `bluetooth` | `bluetooth` | Bluetooth device scanning, pairing, and connection |
| `display` | `display` | Display output configuration (resolution, refresh rate) |
| `printer` | `printer` | Print job submission and management via CUPS |
| `raw_usb` | `raw-usb` | Direct USB device access (bulk/interrupt/control transfers) |
| `usb_storage` | `usb-storage` | USB mass storage mount/unmount/eject via UDisks2 |
| `webcam` | `webcam` | Webcam image and burst capture via Video4Linux |
| `mqtt` | `mqtt` | MQTT broker bridge — publish, subscribe, and back IoT device twins |
| `homeassistant` | `homeassistant` | Home Assistant integration — enumerate entities, set states, observe events |

### Device Twins & Safety Engine

IoT drivers represent each addressable entity as a **device twin** with a desired state and a reported state.

- **DesiredStateSet** — an agent updates the desired state of a twin via the corresponding HAL action. The safety engine evaluates the requested value against per-device rules (range checks, rate limits, allowlists) before forwarding it to the device.
- **SafetyRuleViolation** — emitted when the safety engine refuses a desired-state change. The actuator command is not sent.
- **ReportedStateUpdated** — emitted when the device reports a new measured value (sensor reading, switch state). Twins remember the last reported state so agents can read it without re-querying the device.

Use `hal register --twin <id>` to register a twin (also done automatically by the `mqtt` and `homeassistant` drivers when they discover a new entity).

Feature-gated drivers are compiled only when their feature flag is enabled:

```bash
# Build with specific HAL drivers
cargo build -p agentos-kernel --features audio,bluetooth,webcam

# Build with all peripheral drivers
cargo build -p agentos-kernel --features audio,bluetooth,display,printer,raw-usb,usb-storage,webcam
```

### Consent Store

Privacy-sensitive HAL drivers (webcam, audio, bluetooth) require explicit consent before accessing hardware. The `ConsentStore` (`crates/agentos-hal/src/consent.rs`) manages time-limited consent grants:

| Operation | Description |
|-----------|-------------|
| `grant(agent_id, resource, ttl)` | Grant access to a resource for a specified duration |
| `check(agent_id, resource)` | Check if an active, non-expired grant exists |
| `revoke(agent_id, resource)` | Immediately revoke access |
| `list()` | List all active grants with remaining TTL |

Grants are keyed by `(agent_id, resource)` and automatically expire after their TTL. A background prune removes expired grants on every check. Example resources: `hardware.webcam.capture`, `hardware.audio.record`, `hardware.bluetooth.scan`.

### Audio Driver

The audio driver (`crates/agentos-hal/src/drivers/audio.rs`) provides capture and playback via PipeWire or PulseAudio.

| Action | Description | Key Params |
|--------|-------------|------------|
| `list` | List audio sources and sinks | None |
| `capture` | Record audio from a source | `source`, `duration_seconds`, `sample_rate` |
| `playback` | Play an audio file to a sink | `sink`, `audio_path` |

**Permission:** `hardware.audio:x` — **Events:** `AudioCaptureStarted`, `AudioCaptureStopped`, `AudioPlaybackStarted`

### Bluetooth Driver

The Bluetooth driver (`crates/agentos-hal/src/drivers/bluetooth.rs`) provides device discovery, pairing, and connection.

| Action | Description | Key Params |
|--------|-------------|------------|
| `scan` | Scan for nearby Bluetooth devices | `duration_seconds` |
| `pair` | Initiate pairing with a device | `device_address` |
| `connect` | Connect to a paired device | `device_address` |
| `list` | List known/paired devices | None |

**Permission:** `hardware.bluetooth:x` — **Events:** `BluetoothScanStarted`, `BluetoothPairRequested`, `BluetoothConnected`

### Display Driver

The display driver (`crates/agentos-hal/src/drivers/display.rs`) manages display output configuration.

| Action | Description | Key Params |
|--------|-------------|------------|
| `list` | List connected display outputs | None |
| `configure` | Apply resolution/refresh rate | `output`, `width`, `height`, `refresh_rate` |
| `revert` | Revert to previous configuration | `output` |

**Permission:** `hardware.display:x` — **Events:** `DisplayConfigApplied`, `DisplayConfigReverted`

### Printer Driver

The printer driver (`crates/agentos-hal/src/drivers/printer.rs`) submits and manages print jobs via CUPS.

| Action | Description | Key Params |
|--------|-------------|------------|
| `list` | List available printers | None |
| `print` | Submit a print job | `printer`, `document_path`, `job_name` |
| `cancel` | Cancel a print job | `printer`, `job_id` |
| `status` | Check printer/job status | `printer` |

**Permission:** `hardware.printer:x` — **Audit events:** `PrintJobSubmitted`, `PrintJobCancelled`

### Raw USB Driver

The raw USB driver (`crates/agentos-hal/src/drivers/raw_usb.rs`) provides direct device access for bulk, interrupt, and control transfers.

| Action | Description | Key Params |
|--------|-------------|------------|
| `list` | List USB devices | None |
| `open` | Open a USB device for transfers | `vendor_id`, `product_id`, `interface` |
| `transfer` | Perform a USB transfer | `device_key`, `transfer_kind`, `endpoint`, `direction` |

**Permission:** `hardware.raw-usb:x` — **Events:** `RawUsbDeviceOpened`, `RawUsbTransferCompleted`

### Webcam Driver

The webcam driver (`crates/agentos-hal/src/drivers/webcam.rs`) captures images and burst sequences via Video4Linux.

| Action | Description | Key Params |
|--------|-------------|------------|
| `list` | List webcam devices | None |
| `capture` | Capture a single frame | `device`, `width`, `height`, `format` |
| `burst` | Capture multiple frames | `device`, `count`, `interval_ms` |

**Permission:** `hardware.webcam:x` — **Events:** `WebcamCaptureStarted`, `WebcamCaptureStopped`

### USB Storage Driver

The `UsbStorageDriver` enables agents to mount, unmount, eject, and list USB storage devices via the UDisks2 D-Bus API. It is feature-gated behind `usb-storage` and must be compiled explicitly.

#### Enabling

```bash
# Build the HAL crate with USB support
cargo build -p agentos-hal --features usb-storage

# Build the kernel with USB support (propagates to HAL)
cargo build -p agentos-kernel --features usb-storage
```

When the feature is disabled, the driver module is excluded entirely — no `zbus` dependency is pulled in.

#### Actions

| Action | Description | Required Params |
|--------|-------------|-----------------|
| `list` | List USB-backed filesystems via UDisks2 ObjectManager | None |
| `mount` | Mount a USB filesystem with safe options (`nosuid,noexec,nodev`) | `device` |
| `unmount` | Unmount a USB filesystem | `device` |
| `eject` | Power off the parent USB drive | `device` |

#### Usage via `agentos`

```bash
# List all USB filesystems
agentos hal query usb-storage '{"action": "list"}'

# Mount a USB drive partition
agentos hal query usb-storage '{"action": "mount", "device": "sdb1"}'

# Unmount
agentos hal query usb-storage '{"action": "unmount", "device": "sdb1"}'

# Eject (power off the drive)
agentos hal query usb-storage '{"action": "eject", "device": "sdb1"}'
```

#### Security

- **Permission required:** `hardware.usb-storage:x` (Execute) — enforced by the HAL driver trait
- **Device quarantine:** The HAL device registry gates access; the device key `usb-storage:<device>` must be approved for the requesting agent before any operation proceeds
- **USB-only enforcement:** Before mount, unmount, or eject, the driver reads the UDisks2 `Drive.ConnectionBus` property and rejects any device where the bus is not `"usb"`
- **Device name validation:** Only alphanumeric characters, hyphens (`-`), underscores (`_`), and dots (`.`) are allowed; path traversal sequences (`..`) and slashes are rejected before any D-Bus call is made
- **Safe mount options:** All mounts use `nosuid,noexec,nodev` — agents cannot execute binaries or create device nodes on mounted USB drives

#### Audit Events

Successful USB operations emit events into the kernel event/audit pipeline:

| Event Type | Trigger | Payload |
|------------|---------|---------|
| `DeviceMounted` | Successful mount | `driver`, `device`, `mount_path` |
| `DeviceUnmounted` | Successful unmount | `driver`, `device` |
| `DeviceEjected` | Successful eject/power-off | `driver`, `device` |

These events are signed and recorded in the append-only audit log.

#### Limitations

- Requires a running UDisks2 daemon on the host (standard on most desktop Linux distributions)
- Communicates via the system D-Bus — the AgentOS process must have permission to call UDisks2 methods (typically requires `polkit` authorization or running as a user in the `plugdev`/`disk` group)
- Loopback devices report `ConnectionBus` as empty, so they are rejected by the USB-only check; only physical USB-backed block devices are accepted

---

## Resource Arbitration

The `ResourceArbiter` (`crates/agentos-kernel/src/resource_arbiter.rs`) enforces shared/exclusive locking on named resources to prevent concurrent conflicts between agents running in parallel (Spec §8).

### Lock Modes

| Mode | Behaviour |
|---|---|
| `Shared` | Multiple agents can hold a shared lock simultaneously (read-only access) |
| `Exclusive` | Only one agent can hold the lock at a time (read/write access) |

A `Shared` lock blocks any new `Exclusive` request. An `Exclusive` lock blocks all other requests.

### Resource ID Format

Resources are identified by strings. Convention:

- `fs:/path/to/file` — filesystem paths
- `browser:0` — browser slot 0
- `api:<service>` — external API rate-limited slot

### FIFO Waiter Queue

When a lock cannot be immediately granted, the requesting agent is placed in a FIFO queue for that resource. When the current holder releases the lock, the next eligible waiter is woken and granted the lock. For shared locks, multiple consecutive shared waiters are woken simultaneously.

### Deadlock Detection

Before queuing any waiter, the arbiter checks for deadlock using a DFS cycle scan on the wait-for graph (`agent → agent-it-is-blocked-on`). If adding the new wait edge would create a cycle, the request is rejected with an error immediately.

Example: Agent A holds `res1`, Agent B holds `res2`. B waits on `res1` (queued). If A then tries to acquire `res2`, the wait-for graph would have A→B→A — a cycle. The arbiter detects this and returns `Err("Deadlock detected: ...")`.

**Priority-based preemption:** If the deadlocked requester has a higher priority than the current holder, the holder is preempted (its lock forcibly released) and the requester is granted the lock. This is used for high-priority system tasks that must not deadlock.

### TTL (Auto-Release)

Locks can have a TTL in seconds. A background sweep (`sweep_expired()`) runs every 10 minutes and releases any locks that have exceeded their TTL. TTL of `0` means no auto-release.

### Resource CLI

```bash
# List all currently held resource locks
agentos resource list

# Show resource contention (waiters, blocked agents)
agentos resource contention

# Forcibly release a specific lock
agentos resource release --resource fs:/var/data/report.md --agent researcher

# Release all locks held by an agent
agentos resource release-all --agent researcher
```

Output of `agentos resource list`:

```
Resource                       Mode       Held By              TTL(s)
--------------------------------------------------------------------------
fs:/var/data/report.md         exclusive  researcher           30
fs:/var/data/summary.csv       shared     coder, analyst       0
```

---

## Snapshots and Rollback

Context snapshots save a complete serialized copy of a task's context window so it can be restored later.

### Auto-Snapshot Triggers

The kernel takes a snapshot automatically before:

1. **Write operations** — any tool execution that modifies persistent state (file writes, secret creation)
2. **Budget exhaustion** — when a task's token budget is about to be exceeded

This ensures every destructive or budget-constrained operation has a safe rollback point.

### Snapshot Expiry

A background sweep runs every 10 minutes and deletes snapshots older than 72 hours. This prevents unbounded growth of the audit database. Expired snapshots emit a `SnapshotExpired` audit event.

### Listing Snapshots

```bash
agentos snapshot list --task <task-id>
```

Output:

```
SNAPSHOT_REF                             ACTION               SIZE         CREATED
snap_0001                                before_write         4096         1742205781
snap_0002                                budget_limit         4128         1742205892

Total: 2 snapshot(s)
```

The same data is accessible via:

```bash
agentos audit snapshots --task <task-id>
```

### Rolling Back

```bash
# Roll back to the most recent snapshot
agentos snapshot rollback --task <task-id>

# Roll back to a specific snapshot
agentos snapshot rollback --task <task-id> --snapshot snap_0001
```

Also accessible via:

```bash
agentos audit rollback --task <task-id> [--snapshot <ref>]
```

After rollback, the task context is restored to the snapshot state. The task can then resume or be resubmitted.

---

## Escalation Management

Escalations are created when a risk classifier scores an agent action at Level 3 or Level 4. The task is paused and waits for a human operator decision.

### When Escalations Are Created

- **Level 3** — high-risk action requiring human review before proceeding
- **Level 4** — critical action (e.g., destructive file operations, external API calls with irreversible effects)

The escalation record includes the task context, agent ID, the specific action being blocked, a risk summary, and available decision options.

### Auto-Expiry

Escalations that are not resolved within **5 minutes** are automatically denied. The paused task receives a rejection and can handle it as an error or retry.

### Escalation CLI

```bash
# List pending escalations
agentos escalation list

# List all escalations including resolved ones
agentos escalation list --all

# Show details of a specific escalation
agentos escalation get <id>

# Resolve an escalation with a decision
agentos escalation resolve <id> --decision "Approved"
agentos escalation resolve <id> --decision "Denied"
agentos escalation resolve <id> --decision "Acknowledged"
```

Output of `agentos escalation list`:

```
ID     TASK         URGENCY    BLOCKING   STATUS   SUMMARY
----------------------------------------------------------------------
42     abc12345     high       yes        pending  Agent wants to delete all files in /var/...
43     def67890     medium     no         pending  Agent wants to call external payment API...
```

Output of `agentos escalation get 42`:

```
Escalation #42
============================================================
Task ID:      abc12345-...
Agent ID:     coder
Reason:       High-risk file deletion in system directory
Urgency:      high
Blocking:     yes
Status:       pending

Summary:
  Agent is attempting to delete 47 files under /var/lib/...

Decision point:
  Should the agent proceed with bulk file deletion?

Options:
  - Approve
  - Deny
  - Request confirmation for each file
```

### Resolution

When an escalation is resolved, the kernel receives the decision and resumes the paused task with the decision injected into its context. The task can then act on the approval or denial.

```
Escalation #42 resolved: Approved
Task abc12345 resumed.
```

---

## Identity Management

Each agent has an Ed25519 keypair used for cryptographic identity. The keypair is generated when the agent is first connected and stored securely in the kernel.

### Viewing an Agent's Identity

```bash
agentos identity show --agent <name>
```

Output:

```
Agent:       coder
ID:          a7f3b2c1-...
Public Key:  ed25519:3a7f9b2c4d1e...
Signing Key: present
```

The public key is safe to share. The signing key (private key) is held only in kernel memory and is never exported.

### Revoking an Identity

```bash
agentos identity revoke --agent <name>
```

This permanently revokes the agent's cryptographic identity and all associated permissions. The agent will need to be reconnected to generate a new keypair and receive new permissions.

```
Identity and permissions revoked for agent 'coder'.
```

Revocation is useful when:

- An agent is suspected of compromise
- An agent's role changes and old permissions must be cleared
- Cleaning up a decommissioned agent

---

## Related

- [[14-Audit Log]] — audit events for all advanced operations
- [[08-Security Model]] — capability tokens and permission enforcement
- [[16-Configuration Reference]] — relevant config keys
- [[15-LLM Configuration]] — agent connection and provider setup
