use agentos_types::AgentOSError;
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Read a single length-prefixed JSON message from a stream.
pub async fn read_message<T: DeserializeOwned>(stream: &mut UnixStream) -> Result<T, AgentOSError> {
    // Read 4-byte length prefix (big-endian u32)
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| AgentOSError::BusError(format!("Failed to read message length: {}", e)))?;
    let len = u32::from_be_bytes(len_buf) as usize;

    // Sanity check: max message size 16 MB
    if len > 16 * 1024 * 1024 {
        return Err(AgentOSError::BusError(format!(
            "Message too large: {} bytes",
            len
        )));
    }

    // Read the JSON payload
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| AgentOSError::BusError(format!("Failed to read message payload: {}", e)))?;

    serde_json::from_slice(&buf).map_err(|e| AgentOSError::Serialization(e.to_string()))
}

/// Write a single length-prefixed JSON message to a stream.
pub async fn write_message<T: Serialize>(
    stream: &mut UnixStream,
    msg: &T,
) -> Result<(), AgentOSError> {
    let json = serde_json::to_vec(msg).map_err(|e| AgentOSError::Serialization(e.to_string()))?;

    let len = json.len() as u32;
    stream
        .write_all(&len.to_be_bytes())
        .await
        .map_err(|e| AgentOSError::BusError(format!("Failed to write length prefix: {}", e)))?;
    stream
        .write_all(&json)
        .await
        .map_err(|e| AgentOSError::BusError(format!("Failed to write message payload: {}", e)))?;
    stream
        .flush()
        .await
        .map_err(|e| AgentOSError::BusError(format!("Failed to flush stream: {}", e)))?;
    Ok(())
}
