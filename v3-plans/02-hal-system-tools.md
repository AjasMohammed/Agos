# Plan 02 — Hardware Abstraction Layer & System Tools (`agentos-hal`)

## Goal

Build the Hardware Abstraction Layer (HAL) — the kernel subsystem that mediates all agent access to physical and virtual OS resources. Then implement the system-inspection tools that sit on top of it: `sys-monitor`, `process-manager`, `log-reader`, `network-monitor`, and `hardware-info`.

---

## Why the HAL Matters

Without the HAL, agents have no way to observe the system they're running on. These are the tools that make AgentOS useful for real operational tasks: summarising error logs, monitoring system health, managing runaway processes.

The HAL also enforces the spec's contract: **agents never touch raw device files** (`/dev/...`). They interact with typed, permission-gated abstractions the kernel mediates.

---

## Dependencies

```toml
# New workspace dependencies
sysinfo = "0.33"      # CPU, RAM, disk, process enumeration (cross-platform)
regex   = "1"         # Already present in kernel — log parsing patterns
```

---

## New Crate: `agentos-hal`

```
crates/agentos-hal/
├── Cargo.toml
└── src/
    ├── lib.rs               # pub use drivers::{...}; pub use hal::HardwareAbstractionLayer
    ├── hal.rs               # HardwareAbstractionLayer orchestrator
    ├── drivers/
    │   ├── mod.rs
    │   ├── system.rs        # CPU, RAM, uptime, OS info
    │   ├── process.rs       # Process list, kill
    │   ├── storage.rs       # Disk usage per mount
    │   ├── network.rs       # Interface stats, connection table
    │   └── log_reader.rs    # Structured log file reading
    └── types.rs             # SystemSnapshot, ProcessEntry, etc.
```

---

## Core HAL Trait

```rust
/// Every HAL driver implements this trait.
#[async_trait]
pub trait HalDriver: Send + Sync {
    /// Human-readable driver name (e.g. "system", "process").
    fn name(&self) -> &str;

    /// The permission required to use this driver.
    fn required_permission(&self) -> (&str, PermissionOp);

    /// Execute a typed query and return a JSON result.
    async fn query(&self, params: serde_json::Value) -> Result<serde_json::Value, AgentOSError>;
}

pub struct HardwareAbstractionLayer {
    drivers: HashMap<String, Box<dyn HalDriver>>,
}

impl HardwareAbstractionLayer {
    pub fn new() -> Self;
    pub fn register(&mut self, driver: Box<dyn HalDriver>);
    pub async fn query(
        &self,
        driver_name: &str,
        params: serde_json::Value,
        permission_check: &PermissionSet,
    ) -> Result<serde_json::Value, AgentOSError>;
}
```

---

## HAL Drivers

### `SystemDriver` — `hardware.system:r`

Uses `sysinfo::System` to collect:

```rust
pub struct SystemSnapshot {
    pub cpu_usage_percent: f32,         // Overall CPU usage
    pub cpu_core_count: usize,
    pub memory_total_mb: u64,
    pub memory_used_mb: u64,
    pub memory_available_mb: u64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    pub uptime_seconds: u64,
    pub os_name: String,                // e.g. "Linux"
    pub os_version: String,             // e.g. "6.8.0"
    pub hostname: String,
    pub load_average: (f64, f64, f64),  // 1m, 5m, 15m
    pub disk_usage: Vec<DiskInfo>,      // per mount point
}
```

### `ProcessDriver` — `process.list:r` / `process.kill:x`

```rust
pub struct ProcessEntry {
    pub pid: u32,
    pub name: String,
    pub cpu_usage_percent: f32,
    pub memory_mb: u64,
    pub status: String,       // Running, Sleeping, Zombie, etc.
    pub parent_pid: Option<u32>,
    pub start_time: DateTime<Utc>,
    pub command: String,      // Full command line
}

// list_processes() → Vec<ProcessEntry>
// kill_process(pid: u32, signal: KillSignal) → Result<(), AgentOSError>
```

Kill requires `process.kill:x`. Sending `SIGKILL` requires an additional audit log entry.

### `NetworkDriver` — `network.logs:r`

```rust
pub struct NetworkInterface {
    pub name: String,             // eth0, lo, wlan0
    pub ip_addresses: Vec<String>,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub packets_received: u64,
    pub packets_sent: u64,
    pub errors_in: u64,
    pub errors_out: u64,
}
```

No raw packet capture — read-only stats only.

### `LogReaderDriver` — `fs.app_logs:r` or `fs.system_logs:r`

```rust
pub struct LogQuery {
    pub source: LogSource,        // AppLog(path) | SystemLog | KernelLog
    pub last_n_lines: Option<u64>,
    pub since: Option<DateTime<Utc>>,
    pub grep_pattern: Option<String>, // Regex filter
    pub level_filter: Option<Vec<LogLevel>>, // ERROR, WARN, INFO
}

pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
    pub source: String,
}
```

---

## System Tools (HAL-backed)

### `sys-monitor`

| Property            | Value               |
| ------------------- | ------------------- |
| Permission required | `hardware.system:r` |
| Input               | (none)              |
| Output              | `SystemSnapshot`    |

Calls `SystemDriver::query()`.

### `process-manager`

| Property            | Value                                              |
| ------------------- | -------------------------------------------------- |
| Permission required | `process.list:r` (list) / `process.kill:x` (kill)  |
| Input               | `{ action: "list" \| "kill", pid?: u32 }`          |
| Output              | `Vec<ProcessEntry>` or confirmation                |

### `log-reader`

| Property            | Value                                 |
| ------------------- | ------------------------------------- |
| Permission required | `fs.app_logs:r` or `fs.system_logs:r` |
| Input               | `LogQuery`                            |
| Output              | `Vec<LogEntry>`                       |

### `network-monitor`

| Property            | Value                    |
| ------------------- | ------------------------ |
| Permission required | `network.logs:r`         |
| Input               | (none or interface name) |
| Output              | `Vec<NetworkInterface>`  |

### `hardware-info`

| Property            | Value                                                        |
| ------------------- | ------------------------------------------------------------ |
| Permission required | `hardware.system:r`                                          |
| Input               | (none)                                                       |
| Output              | CPU model, core count, GPU devices (if any), memory, storage |

---

## Integration Points

### `agentos-kernel/src/kernel.rs`

```rust
// In Kernel struct:
hal: Arc<HardwareAbstractionLayer>,

// In boot():
let mut hal = HardwareAbstractionLayer::new();
hal.register(Box::new(SystemDriver::new()));
hal.register(Box::new(ProcessDriver::new()));
hal.register(Box::new(NetworkDriver::new()));
hal.register(Box::new(LogReaderDriver::new(config.hal.log_paths.clone())));
let hal = Arc::new(hal);
```

### `agentos-kernel/Cargo.toml`

```toml
agentos-hal = { path = "../agentos-hal" }
```

---

## File Layout for Tools

```
crates/agentos-tools/src/
├── sys_monitor.rs        # SysMonitorTool → calls hal.query("system", ...)
├── process_manager.rs    # ProcessManagerTool → calls hal.query("process", ...)
├── log_reader.rs         # LogReaderTool → calls hal.query("log", ...)
├── network_monitor.rs    # NetworkMonitorTool → calls hal.query("network", ...)
└── hardware_info.rs      # HardwareInfoTool → calls hal.query("system", ...)
```

Each tool simply:

1. Deserialises the payload
2. Checks its specific permission
3. Delegates to the HAL driver
4. Returns the structured result

---

## Tests

```rust
// agentos-hal/src/drivers/system.rs
#[test]
fn test_system_snapshot_has_required_fields() {
    let driver = SystemDriver::new();
    let snapshot: SystemSnapshot = driver.snapshot().unwrap();
    assert!(snapshot.cpu_core_count > 0);
    assert!(snapshot.memory_total_mb > 0);
}

// Integration: process list
#[tokio::test]
async fn test_process_list_returns_self() {
    let driver = ProcessDriver::new();
    let procs = driver.list_processes().await.unwrap();
    let self_pid = std::process::id();
    assert!(procs.iter().any(|p| p.pid == self_pid));
}

// Permission gate
#[tokio::test]
async fn test_process_kill_requires_permission() {
    let hal = HardwareAbstractionLayer::new_with_defaults();
    let perms = PermissionSet::empty(); // no kill:x
    let result = hal.query("process", json!({"action": "kill", "pid": 99999}), &perms).await;
    assert!(matches!(result, Err(AgentOSError::PermissionDenied(_))));
}
```

---

## Verification

```bash
# Build the hal crate
cargo build -p agentos-hal

# Grant system permission and test via CLI
agentctl perm grant analyst hardware.system:r process.list:r

agentctl task run --agent analyst "What is the current CPU usage and memory available?"
agentctl task run --agent analyst "List all running processes sorted by memory usage"
agentctl task run --agent analyst "Show me the last 50 error lines from the application log"
```

> [!NOTE]
> The `process.kill:x` permission is off by default for ALL agents. Granting it should produce an explicit audit log entry and an operator confirmation prompt in the CLI.
