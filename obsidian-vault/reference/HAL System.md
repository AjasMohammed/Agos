---
title: HAL System
tags: [reference, hal, hardware]
---

# Hardware Abstraction Layer (HAL)

The HAL provides a pluggable driver pattern for agents to query system resources through a uniform interface.

**Source:** `crates/agentos-hal/src/`

## HalDriver Trait

```rust
#[async_trait]
pub trait HalDriver: Send + Sync {
    fn name(&self) -> &str;
    fn required_permission(&self) -> (&str, PermissionOp);
    async fn query(&self, params: Value) -> Result<Value, AgentOSError>;
}
```

Every driver enforces its required permission before responding to queries.

## Built-in Drivers

### SystemDriver
- **Permission:** `hardware.system:r`
- **Queries:** CPU info, memory usage, uptime, kernel version, load averages

### ProcessDriver
- **Permission:** `process.list:r` (list), `process.kill:x` (kill)
- **Queries:** List processes, get process info, kill process by PID

### NetworkDriver
- **Permission:** `network.logs:r`
- **Queries:** Network interface stats, connection counts

### LogReaderDriver
- **Permission:** `fs.app_logs:r`
- **Queries:** Read syslog entries, filter by level/source

## Integration

The HAL is initialized during kernel boot and passed to tools via `ToolExecutionContext`:

```
Kernel Boot → Create HAL → Register Drivers → Pass to ToolRunner
                                                    ↓
                                         Tools access via context.hal
```

Tools like `sys-monitor`, `process-manager`, `network-monitor`, `log-reader`, and `hardware-info` use the HAL to query system state.
