---
title: Systemd Unit and Watchdog
tags:
  - deployment
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 3h
priority: medium
---

# Systemd Unit and Watchdog

> Create a production-grade systemd unit file with watchdog integration, resource limits, and proper restart policy. Optionally add watchdog notification to the kernel's health server.

---

## Why This Subtask

The kernel has no external process supervisor. When the internal supervisor exhausts its restart budget and exits, nothing brings it back. For bare-metal or VM deployments, systemd is the standard process manager. The Dockerfile already exists for container deployments, but there is no systemd path.

Key requirements:
1. `Restart=on-failure` to recover from crash exits
2. `WatchdogSec` to detect kernel hangs (not just crashes)
3. Resource limits to prevent runaway memory/CPU from affecting the host
4. Proper ordering: start after network, before services that depend on AgentOS

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Process management | None (manual start) | systemd unit with `Restart=on-failure` |
| Hang detection | None | systemd watchdog (`WatchdogSec=60s`) with kernel health server pinging `sd_notify` |
| Resource limits | None | `MemoryMax=4G`, `CPUQuota=200%` (configurable) |
| Log integration | stderr only | `StandardOutput=journal`, `StandardError=journal` |

## What to Do

### Part 1: Systemd Unit File

1. Create `deploy/agentos.service`:

```ini
[Unit]
Description=AgentOS Kernel — LLM-native operating system for AI agents
Documentation=https://github.com/your-org/agentos
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExecStart=/usr/local/bin/agentctl start --config /etc/agentos/config.toml
ExecStop=/usr/local/bin/agentctl stop

# Graceful shutdown: send SIGTERM, wait 30s, then SIGKILL
TimeoutStopSec=30
KillMode=mixed
KillSignal=SIGTERM

# Restart policy: restart on failure, with increasing delays
Restart=on-failure
RestartSec=5s
# Maximum 5 restarts within a 5-minute window
StartLimitIntervalSec=300
StartLimitBurst=5

# Watchdog: kernel must ping within this interval or systemd restarts it
WatchdogSec=60s

# Resource limits
MemoryMax=4G
MemoryHigh=3G
CPUQuota=200%
TasksMax=512

# Security hardening
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/agentos /var/log/agentos /run/agentos
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes

# Environment
Environment=RUST_LOG=info
Environment=AGENTOS_VAULT_PASSPHRASE_FILE=/etc/agentos/vault-passphrase
EnvironmentFile=-/etc/agentos/environment

# Working directory
WorkingDirectory=/var/lib/agentos

# User/group (create with: useradd -r -s /sbin/nologin agentos)
User=agentos
Group=agentos

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=agentos

[Install]
WantedBy=multi-user.target
```

### Part 2: Watchdog Notification (Optional Enhancement)

2. If `sd_notify` integration is desired, add it to the kernel health server. Open `crates/agentos-kernel/src/health.rs` (check the actual file name).

The health server already runs periodic checks. Add a watchdog ping after each successful check cycle:

```rust
/// Notify systemd that the process is alive (watchdog ping).
/// This is a no-op if not running under systemd or if WatchdogSec is not set.
fn notify_watchdog() {
    #[cfg(target_os = "linux")]
    {
        // Use sd_notify protocol: write "WATCHDOG=1" to the notification socket
        if let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") {
            use std::os::unix::net::UnixDatagram;
            if let Ok(sock) = UnixDatagram::unbound() {
                let _ = sock.send_to(b"WATCHDOG=1", &socket_path);
            }
        }
    }
}

/// Notify systemd that the service is ready (called once after boot completes).
pub fn notify_ready() {
    #[cfg(target_os = "linux")]
    {
        if let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") {
            use std::os::unix::net::UnixDatagram;
            if let Ok(sock) = UnixDatagram::unbound() {
                let _ = sock.send_to(b"READY=1", &socket_path);
            }
        }
    }
}
```

3. Call `notify_ready()` at the end of `Kernel::boot()` in `kernel.rs`, just before returning `Ok(kernel)`.

4. Call `notify_watchdog()` in the health check HTTP handler or at the end of each health monitor cycle.

### Part 3: Installation Documentation

5. Create `deploy/README-systemd.md` (only if the user wants this -- this is a deployment doc, not an obsidian doc):

Alternatively, document the installation steps in the systemd unit file comments.

Key installation steps:
```bash
# Create system user
sudo useradd -r -s /sbin/nologin -d /var/lib/agentos agentos

# Create directories
sudo mkdir -p /var/lib/agentos /var/log/agentos /run/agentos /etc/agentos
sudo chown agentos:agentos /var/lib/agentos /var/log/agentos /run/agentos

# Copy config
sudo cp config/production.toml /etc/agentos/config.toml

# Install binary
sudo cp target/release/agentctl /usr/local/bin/

# Install unit file
sudo cp deploy/agentos.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable agentos
sudo systemctl start agentos
```

## Files Changed

| File | Change |
|------|--------|
| `deploy/agentos.service` (new) | Systemd unit file with watchdog, resource limits, security hardening |
| `crates/agentos-kernel/src/health.rs` | Add `notify_watchdog()` and `notify_ready()` functions |
| `crates/agentos-kernel/src/kernel.rs` | Call `notify_ready()` at end of `boot()` |

## Prerequisites

All other phases should ideally be complete (graceful shutdown, pre-flight checks, restart hardening) for the systemd unit to work optimally. However, the unit file itself can be created at any time.

## Test Plan

- `systemd-analyze verify deploy/agentos.service` passes (syntax validation)
- `cargo build -p agentos-kernel` compiles with the new health functions
- Manual test: start under systemd, verify `systemctl status agentos` shows active
- Manual test: kill the process with `SIGKILL`, verify systemd restarts it
- Manual test: stop with `systemctl stop agentos`, verify `KernelShutdown` audit entry

## Verification

```bash
# Syntax check (does not require running systemd)
test -f deploy/agentos.service
cargo build -p agentos-kernel
cargo clippy -p agentos-kernel -- -D warnings
```
