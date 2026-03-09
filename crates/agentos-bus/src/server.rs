use crate::message::BusMessage;
use crate::transport::{read_message, write_message};
use agentos_types::AgentOSError;
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

pub struct BusServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl Drop for BusServer {
    fn drop(&mut self) {
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

impl BusServer {
    /// Start listening on the configured socket path.
    /// Removes any stale socket file first.
    pub async fn bind(socket_path: &Path) -> Result<Self, AgentOSError> {
        // Remove stale socket file if it exists
        if socket_path.exists() {
            std::fs::remove_file(socket_path).map_err(|e| {
                AgentOSError::BusError(format!("Failed to remove stale socket: {}", e))
            })?;
        }

        let listener = UnixListener::bind(socket_path)
            .map_err(|e| AgentOSError::BusError(format!("Failed to bind to Unix socket: {}", e)))?;

        tracing::info!("Intent Bus listening on {:?}", socket_path);

        Ok(Self {
            listener,
            socket_path: socket_path.to_path_buf(),
        })
    }

    /// Accept a single connection. Returns a BusConnection for reading/writing messages.
    pub async fn accept(&self) -> Result<BusConnection, AgentOSError> {
        let (stream, _addr) =
            self.listener.accept().await.map_err(|e| {
                AgentOSError::BusError(format!("Failed to accept connection: {}", e))
            })?;
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
