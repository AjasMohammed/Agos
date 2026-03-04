# Plan 06 — Intent Bus (`agentos-bus` crate)

## Goal

Implement the IPC layer using Unix domain sockets for communication between the kernel and tools/CLI. This is the transport for `IntentMessage` objects.

## Dependencies

- `agentos-types`
- `tokio` (full features — provides `UnixListener`, `UnixStream`)
- `serde`, `serde_json`
- `bytes`
- `tracing`

## Architecture

```
                 Unix Domain Socket
CLI / Tool  ←─────────────────────────→  Kernel
            │     framed messages     │
            │                         │
            │  Length(4 bytes, u32 BE) │
            │  JSON payload           │
            │                         │
            └─────────────────────────┘
```

The bus uses a simple **length-prefixed JSON** framing protocol over Unix domain sockets:

1. Send: write 4 bytes (u32 big-endian) = length of JSON payload, then write JSON bytes
2. Receive: read 4 bytes for length, then read exactly that many bytes, deserialize JSON

## Wire Protocol

```rust
/// Messages sent over the bus. This is the top-level envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BusMessage {
    /// CLI/tool sends an intent to the kernel
    Intent(IntentMessage),

    /// Kernel sends a result back to CLI/tool
    IntentResult(IntentResult),

    /// CLI sends a command to the kernel (non-intent operations)
    Command(KernelCommand),

    /// Kernel sends a response to a command
    CommandResponse(KernelResponse),

    /// Kernel pushes a status update (for task monitoring)
    StatusUpdate(StatusUpdate),
}

/// Commands from CLI to kernel that aren't task intents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KernelCommand {
    // Agent management
    ConnectAgent {
        name: String,
        provider: LLMProvider,
        model: String,
    },
    ListAgents,
    DisconnectAgent { agent_id: AgentID },

    // Task management
    RunTask {
        agent_name: String,
        prompt: String,
    },
    ListTasks,
    GetTaskLogs { task_id: TaskID },
    CancelTask { task_id: TaskID },

    // Tool management
    ListTools,
    InstallTool { manifest_path: String },
    RemoveTool { tool_name: String },

    // Secret management
    SetSecret {
        name: String,
        value: String,          // encrypted in transit? No — UDS is local-only
        scope: SecretScope,
    },
    ListSecrets,
    RevokeSecret { name: String },
    RotateSecret { name: String, new_value: String },

    // Permission management
    GrantPermission {
        agent_name: String,
        permission: String,    // e.g. "fs.user_data:rw"
    },
    RevokePermission {
        agent_name: String,
        permission: String,
    },
    ShowPermissions { agent_name: String },

    // System
    GetStatus,
    GetAuditLogs { limit: u32 },
    Shutdown,
}

/// Responses from kernel to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KernelResponse {
    Success { data: Option<serde_json::Value> },
    Error { message: String },
    AgentList(Vec<AgentProfile>),
    TaskList(Vec<TaskSummary>),
    TaskLogs(Vec<String>),
    ToolList(Vec<ToolManifest>),
    SecretList(Vec<SecretMetadata>),
    Permissions(PermissionSet),
    Status(SystemStatus),
    AuditLogs(Vec<AuditEntry>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub uptime_secs: u64,
    pub connected_agents: u32,
    pub active_tasks: u32,
    pub installed_tools: u32,
    pub total_audit_entries: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusUpdate {
    pub task_id: TaskID,
    pub state: TaskState,
    pub message: String,
}
```

## Transport Layer

### Framed Reader/Writer

```rust
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Read a single length-prefixed JSON message from a stream.
pub async fn read_message<T: DeserializeOwned>(stream: &mut UnixStream) -> Result<T, AgentOSError> {
    // Read 4-byte length prefix (big-endian u32)
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    // Sanity check: max message size 16 MB
    if len > 16 * 1024 * 1024 {
        return Err(AgentOSError::BusError(
            format!("Message too large: {} bytes", len)
        ));
    }

    // Read the JSON payload
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;

    serde_json::from_slice(&buf)
        .map_err(|e| AgentOSError::Serialization(e.to_string()))
}

/// Write a single length-prefixed JSON message to a stream.
pub async fn write_message<T: Serialize>(stream: &mut UnixStream, msg: &T) -> Result<(), AgentOSError> {
    let json = serde_json::to_vec(msg)
        .map_err(|e| AgentOSError::Serialization(e.to_string()))?;

    let len = json.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&json).await?;
    stream.flush().await?;
    Ok(())
}
```

### Server (Kernel Side)

```rust
use tokio::net::UnixListener;

pub struct BusServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl BusServer {
    /// Start listening on the configured socket path.
    /// Removes any stale socket file first.
    pub async fn bind(socket_path: &Path) -> Result<Self, AgentOSError> {
        // Remove stale socket file if it exists
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        tracing::info!("Intent Bus listening on {:?}", socket_path);

        Ok(Self {
            listener,
            socket_path: socket_path.to_path_buf(),
        })
    }

    /// Accept a single connection. Returns a BusConnection for reading/writing messages.
    pub async fn accept(&self) -> Result<BusConnection, AgentOSError> {
        let (stream, _addr) = self.listener.accept().await?;
        Ok(BusConnection { stream })
    }
}

/// A single bidirectional connection over UDS.
pub struct BusConnection {
    stream: UnixStream,
}

impl BusConnection {
    pub async fn read(&mut self) -> Result<BusMessage, AgentOSError> {
        read_message(&mut self.stream).await
    }

    pub async fn write(&mut self, msg: &BusMessage) -> Result<(), AgentOSError> {
        write_message(&mut self.stream, msg).await
    }
}
```

### Client (CLI Side)

```rust
pub struct BusClient {
    stream: UnixStream,
}

impl BusClient {
    /// Connect to the kernel's bus socket.
    pub async fn connect(socket_path: &Path) -> Result<Self, AgentOSError> {
        let stream = UnixStream::connect(socket_path).await
            .map_err(|e| AgentOSError::BusError(
                format!("Cannot connect to kernel at {:?}: {}. Is the kernel running?", socket_path, e)
            ))?;
        Ok(Self { stream })
    }

    /// Send a command and wait for a response.
    pub async fn send_command(&mut self, cmd: KernelCommand) -> Result<KernelResponse, AgentOSError> {
        write_message(&mut self.stream, &BusMessage::Command(cmd)).await?;

        let response: BusMessage = read_message(&mut self.stream).await?;
        match response {
            BusMessage::CommandResponse(resp) => Ok(resp),
            other => Err(AgentOSError::BusError(
                format!("Unexpected response type: {:?}", std::mem::discriminant(&other))
            )),
        }
    }
}
```

## Socket Path

Default: `/tmp/agentos-kernel.sock` (configurable via `config/default.toml`)

On kernel startup:

1. Remove stale socket file if present
2. Bind `UnixListener` to the path
3. Spawn a tokio task to accept connections in a loop

On kernel shutdown:

1. Stop accepting new connections
2. Close all active connections
3. Remove the socket file

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_server_client_roundtrip() {
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        let server = BusServer::bind(&sock_path).await.unwrap();

        // Spawn server acceptor
        let sock_path_clone = sock_path.clone();
        let server_handle = tokio::spawn(async move {
            let mut conn = server.accept().await.unwrap();
            let msg = conn.read().await.unwrap();
            match msg {
                BusMessage::Command(KernelCommand::GetStatus) => {
                    conn.write(&BusMessage::CommandResponse(
                        KernelResponse::Status(SystemStatus {
                            uptime_secs: 42,
                            connected_agents: 1,
                            active_tasks: 0,
                            installed_tools: 5,
                            total_audit_entries: 100,
                        })
                    )).await.unwrap();
                }
                _ => panic!("Unexpected message"),
            }
        });

        // Client connects and sends a command
        let mut client = BusClient::connect(&sock_path).await.unwrap();
        let response = client.send_command(KernelCommand::GetStatus).await.unwrap();

        match response {
            KernelResponse::Status(status) => {
                assert_eq!(status.uptime_secs, 42);
                assert_eq!(status.connected_agents, 1);
            }
            _ => panic!("Unexpected response"),
        }

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_large_message() {
        // Test that messages up to 16MB are handled correctly
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        let server = BusServer::bind(&sock_path).await.unwrap();

        // Create a large payload
        let large_data = "x".repeat(1_000_000); // 1MB string

        let server_handle = tokio::spawn(async move {
            let mut conn = server.accept().await.unwrap();
            let msg = conn.read().await.unwrap();
            conn.write(&msg).await.unwrap(); // echo back
        });

        let mut client = BusClient::connect(&sock_path).await.unwrap();
        let cmd = KernelCommand::RunTask {
            agent_name: "test".into(),
            prompt: large_data.clone(),
        };
        write_message(&mut client.stream, &BusMessage::Command(cmd)).await.unwrap();

        let response: BusMessage = read_message(&mut client.stream).await.unwrap();
        // Verify round-trip integrity
        match response {
            BusMessage::Command(KernelCommand::RunTask { prompt, .. }) => {
                assert_eq!(prompt.len(), 1_000_000);
            }
            _ => panic!("Unexpected response"),
        }

        server_handle.await.unwrap();
    }
}
```

## Verification

```bash
cargo test -p agentos-bus
```
