---
title: Message Bus
tags: [reference, bus, ipc]
---

# Message Bus

The Intent Bus (`agentos-bus`) provides Unix domain socket IPC between the CLI and the kernel.

**Source:** `crates/agentos-bus/src/`

## Architecture

```
agentctl (BusClient) ──── Unix Socket ──── Kernel (BusServer)
         └── send(BusMessage) ──────────── recv(BusMessage) ──┘
         └── recv(BusMessage) ◄──────────── send(BusMessage) ──┘
```

## Components

### BusServer
- Listens on Unix domain socket (configurable path)
- Accepts incoming connections from CLI clients
- Each connection handled as a `BusConnection`
- Bidirectional message passing

### BusClient
- CLI-side connector
- Connects to kernel's Unix socket
- Sends `KernelCommand`, receives `KernelResponse`

### BusConnection
- Wraps a connected socket
- Serializes/deserializes `BusMessage` over the wire

## BusMessage Enum

```rust
pub enum BusMessage {
    Intent(IntentMessage),           // Tool/kernel operation request
    IntentResult(IntentResult),      // Tool execution result
    Command(KernelCommand),          // Admin command from CLI
    CommandResponse(KernelResponse), // Response to admin command
    StatusUpdate(StatusUpdate),      // Kernel-pushed status change
}
```

## KernelCommand

35+ command variants covering all operations:

- **Agent:** ConnectAgent, ListAgents, DisconnectAgent
- **Task:** RunTask, ListTasks, GetTaskLogs, CancelTask
- **Tool:** ListTools, InstallTool, RemoveTool
- **Secret:** SetSecret, ListSecrets, RevokeSecret, RotateSecret
- **Permission:** GrantPermission, RevokePermission, ShowPermissions
- **Role:** CreateRole, DeleteRole, ListRoles, AssignRole
- **Schedule:** CreateScheduledJob, ListScheduledJobs, PauseSchedule, ResumeSchedule, DeleteSchedule
- **Background:** RunBackgroundTask, ListBackgroundTasks, KillBackgroundTask
- **Pipeline:** InstallPipeline, PipelineList, PipelineRun, PipelineStatus, PipelineLogs, RemovePipeline
- **System:** GetStatus, AuditLogs

## KernelResponse

```rust
pub enum KernelResponse {
    Success(Value),    // JSON payload with result data
    Error(String),     // Error message
}
```

## Intent Messages

See [[Type System#IntentMessage]] for the full structure. Intent types:

| Type | Purpose |
|---|---|
| `Read` | Read data from a tool/resource |
| `Write` | Write/store data |
| `Execute` | Run a process or command |
| `Query` | Search or query data |
| `Observe` | Monitor or watch a resource |
| `Delegate` | Delegate work to another agent |

## Transport Protocol

Messages are serialized as length-prefixed JSON over the Unix socket:
1. 4-byte big-endian length prefix
2. JSON-encoded `BusMessage` body
3. Bidirectional - both sides can send and receive
