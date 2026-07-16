use agentos_types::AgentOSError;
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum allowed message size (16 MiB).
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Timeout for reading the *payload* of a message once its length prefix has
/// arrived. Bytes that are already in flight should land promptly, so this
/// bounds a peer that announces a length then stalls mid-message.
///
/// NOTE: this is deliberately NOT applied to the initial length-prefix read.
/// Waiting for the *next* message to begin is an open-ended wait by design — a
/// CLI blocking on a synchronous `task run` result (which can take minutes of
/// LLM tool-calling), or the kernel holding an idle connection open during an
/// interactive approval prompt (escalations live for 5 minutes), must not be
/// killed by a short deadline. A dead peer still surfaces immediately as EOF.
const PAYLOAD_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Read a single length-prefixed JSON message from any async stream.
pub async fn read_message<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<T, AgentOSError> {
    // Read the 4-byte length prefix (big-endian u32). No timeout: this is the
    // wait for a message to *start*, which is legitimately unbounded (slow
    // synchronous responses, idle interactive connections). Disconnect returns
    // an `UnexpectedEof` error here, so dead peers are still reaped promptly.
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| AgentOSError::BusError(format!("Failed to read message length: {}", e)))?;
    let len = u32::from_be_bytes(len_buf) as usize;

    // Reject empty messages
    if len == 0 {
        return Err(AgentOSError::BusError(
            "Empty message (length 0)".to_string(),
        ));
    }

    // Sanity check: max message size
    if len > MAX_MESSAGE_SIZE {
        return Err(AgentOSError::BusError(format!(
            "Message too large: {} bytes (max {})",
            len, MAX_MESSAGE_SIZE
        )));
    }

    // Read the JSON payload with a timeout — the bytes are already in flight, so
    // a stall here means a misbehaving/half-dead peer.
    let mut buf = vec![0u8; len];
    tokio::time::timeout(PAYLOAD_IO_TIMEOUT, stream.read_exact(&mut buf))
        .await
        .map_err(|_| AgentOSError::BusError("Read timed out waiting for message payload".into()))?
        .map_err(|e| AgentOSError::BusError(format!("Failed to read message payload: {}", e)))?;

    serde_json::from_slice(&buf).map_err(|e| AgentOSError::Serialization(e.to_string()))
}

/// Write a single length-prefixed JSON message to any async stream.
pub async fn write_message<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    msg: &T,
) -> Result<(), AgentOSError> {
    let json = serde_json::to_vec(msg).map_err(|e| AgentOSError::Serialization(e.to_string()))?;

    // Enforce the same size limit on the write path to prevent silent u32 truncation
    if json.len() > MAX_MESSAGE_SIZE {
        return Err(AgentOSError::BusError(format!(
            "Message too large to send: {} bytes (max {})",
            json.len(),
            MAX_MESSAGE_SIZE
        )));
    }

    let len: u32 = json.len().try_into().map_err(|_| {
        AgentOSError::BusError(format!("Message size {} exceeds u32 range", json.len()))
    })?;
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
