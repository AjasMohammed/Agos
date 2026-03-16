use crate::message::{BusMessage, KernelCommand, KernelResponse};
use crate::transport::{read_message, write_message};
use agentos_types::AgentOSError;
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

/// Timeout for establishing a connection to the kernel socket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct BusClient {
    pub(crate) stream: UnixStream,
}

impl BusClient {
    /// Connect to the kernel's bus socket with a timeout.
    pub async fn connect(socket_path: &Path) -> Result<Self, AgentOSError> {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(socket_path))
            .await
            .map_err(|_| {
                AgentOSError::BusError(format!(
                    "Connection to kernel at {:?} timed out after {:?}. Is the kernel running?",
                    socket_path, CONNECT_TIMEOUT
                ))
            })?
            .map_err(|e| {
                AgentOSError::BusError(format!(
                    "Cannot connect to kernel at {:?}: {}. Is the kernel running?",
                    socket_path, e
                ))
            })?;
        Ok(Self { stream })
    }

    /// Send a command and wait for a response.
    pub async fn send_command(
        &mut self,
        cmd: KernelCommand,
    ) -> Result<KernelResponse, AgentOSError> {
        write_message(&mut self.stream, &BusMessage::Command(cmd)).await?;

        let response: BusMessage = read_message(&mut self.stream).await?;
        match response {
            BusMessage::CommandResponse(resp) => Ok(resp),
            other => Err(AgentOSError::BusError(format!(
                "Unexpected response type: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    // Low level read / write
    pub async fn send_message(&mut self, msg: &BusMessage) -> Result<(), AgentOSError> {
        write_message(&mut self.stream, msg).await
    }

    pub async fn receive_message(&mut self) -> Result<BusMessage, AgentOSError> {
        read_message(&mut self.stream).await
    }
}
