pub mod http;
pub mod stdio;
pub(crate) mod util;

pub use util::{read_line_limited, MAX_MCP_RESPONSE_BYTES};

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::types::{JsonRpcRequest, JsonRpcResponse};

/// Error type for MCP transport operations.
///
/// The variant determines reconnect behavior in the supervisor:
/// - `Connection` and `Timeout` → supervisor should attempt reconnect
/// - `Protocol` → server is alive, no reconnect needed
#[derive(Debug)]
pub enum McpTransportError {
    /// Connection-level failure: broken pipe, process crash, HTTP connection refused.
    Connection(String),
    /// Protocol-level error: JSON-RPC error from a live server.
    Protocol { code: i64, message: String },
    /// Timeout waiting for response.
    Timeout(Duration),
}

impl fmt::Display for McpTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(msg) => write!(f, "MCP connection error: {}", msg),
            Self::Protocol { code, message } => {
                write!(f, "MCP protocol error (code {}): {}", code, message)
            }
            Self::Timeout(d) => write!(f, "MCP request timed out after {:?}", d),
        }
    }
}

impl std::error::Error for McpTransportError {}

impl McpTransportError {
    /// Returns `true` if this error indicates reconnection should be attempted.
    pub fn should_reconnect(&self) -> bool {
        matches!(self, Self::Connection(_) | Self::Timeout(_))
    }
}

/// Factory for creating fresh transport instances.
///
/// Used by the supervisor to create new transports on reconnect (e.g., stdio
/// needs a new child process after the previous one crashed).
#[async_trait]
pub trait McpTransportFactory: Send + Sync {
    /// Create a fresh transport instance.
    async fn create(&self) -> Result<Arc<dyn McpTransport>, McpTransportError>;
}

/// Abstraction over how bytes move between AgentOS and an MCP server.
///
/// Implementations handle the wire protocol (stdio pipes, HTTP POST, etc.)
/// but do NOT own the MCP initialize handshake — that is the supervisor's job.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and await the response.
    async fn send(&self, req: &JsonRpcRequest) -> Result<JsonRpcResponse, McpTransportError>;

    /// Send a JSON-RPC notification (fire-and-forget, no response expected).
    ///
    /// MCP notifications (like `notifications/initialized`) have no `id` field
    /// and the server does not send a response. This method writes the message
    /// without attempting to read a reply.
    async fn send_notification(&self, req: &JsonRpcRequest) -> Result<(), McpTransportError>;

    /// Gracefully close the connection.
    async fn close(&self) -> Result<(), McpTransportError>;

    /// Human-readable transport name for logging (e.g. "stdio:filesystem").
    fn transport_name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_should_reconnect() {
        assert!(McpTransportError::Connection("broken pipe".into()).should_reconnect());
        assert!(McpTransportError::Timeout(Duration::from_secs(30)).should_reconnect());
        assert!(!McpTransportError::Protocol {
            code: -32603,
            message: "internal error".into()
        }
        .should_reconnect());
    }

    #[test]
    fn transport_error_display() {
        let conn = McpTransportError::Connection("broken pipe".into());
        assert!(conn.to_string().contains("broken pipe"));

        let proto = McpTransportError::Protocol {
            code: -32601,
            message: "method not found".into(),
        };
        assert!(proto.to_string().contains("-32601"));

        let timeout = McpTransportError::Timeout(Duration::from_secs(30));
        assert!(timeout.to_string().contains("30"));
    }
}
