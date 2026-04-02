use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use std::sync::Arc;

use super::util::{read_line_limited, MAX_MCP_RESPONSE_BYTES};
use super::{McpTransport, McpTransportError, McpTransportFactory};
use crate::types::{JsonRpcRequest, JsonRpcResponse};

/// Default timeout for a single MCP request/response round-trip.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Holds the stdin/stdout pair for an MCP subprocess connection.
struct StdioConnection {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// Stdio-based MCP transport — spawns a child process and communicates via
/// newline-delimited JSON over stdin/stdout.
///
/// Does NOT perform the MCP initialize handshake. That is the supervisor's
/// responsibility, so it can be re-run on reconnect.
pub struct StdioTransport {
    /// Display name for logging.
    name: String,
    /// Executable to spawn.
    command: String,
    /// Arguments passed to the command.
    args: Vec<String>,
    /// Extra environment variables to pass to the subprocess.
    extra_env: HashMap<String, String>,
    /// Working directory for the subprocess.
    working_dir: Option<PathBuf>,
    /// Per-request timeout.
    timeout: Duration,
    /// Stdin/stdout pair, protected by mutex to prevent interleaving.
    conn: Mutex<StdioConnection>,
    /// Child process handle — kept alive for the transport's lifetime.
    child: Mutex<Child>,
}

impl StdioTransport {
    /// Spawn the MCP server subprocess and return a ready transport.
    ///
    /// Only a minimal set of environment variables is inherited (`PATH`, `HOME`,
    /// `TMPDIR`). Additional vars can be passed via `extra_env`.
    pub async fn spawn(
        name: String,
        command: String,
        args: Vec<String>,
        extra_env: HashMap<String, String>,
        working_dir: Option<PathBuf>,
        timeout_secs: Option<u64>,
    ) -> Result<Self, McpTransportError> {
        let safe_env: Vec<(String, String)> = ["PATH", "HOME", "TMPDIR", "TEMP", "TMP"]
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();

        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env_clear()
            .envs(safe_env)
            .envs(extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .kill_on_drop(true);

        if let Some(ref dir) = working_dir {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|e| {
            McpTransportError::Connection(format!(
                "Failed to spawn MCP server '{}': {}",
                command, e
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            McpTransportError::Connection(format!("Failed to acquire stdin pipe for '{}'", command))
        })?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| {
            McpTransportError::Connection(format!(
                "Failed to acquire stdout pipe for '{}'",
                command
            ))
        })?);

        Ok(Self {
            name,
            command,
            args,
            extra_env,
            working_dir,
            timeout: Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)),
            conn: Mutex::new(StdioConnection { stdin, stdout }),
            child: Mutex::new(child),
        })
    }

    /// The command used to spawn this transport (for reconnect logging).
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The arguments used to spawn this transport.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The extra env vars configured for this transport.
    pub fn extra_env(&self) -> &HashMap<String, String> {
        &self.extra_env
    }

    /// The working directory configured for this transport.
    pub fn working_dir(&self) -> Option<&PathBuf> {
        self.working_dir.as_ref()
    }

    /// The timeout configured for this transport.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[async_trait]
impl super::McpTransport for StdioTransport {
    async fn send(&self, req: &JsonRpcRequest) -> Result<JsonRpcResponse, McpTransportError> {
        let mut conn = self.conn.lock().await;

        let mut line = serde_json::to_string(req).map_err(|e| {
            McpTransportError::Connection(format!("Failed to serialize request: {}", e))
        })?;
        line.push('\n');

        conn.stdin.write_all(line.as_bytes()).await.map_err(|e| {
            McpTransportError::Connection(format!("Failed to write to MCP server: {}", e))
        })?;
        conn.stdin.flush().await.map_err(|e| {
            McpTransportError::Connection(format!("Failed to flush MCP server stdin: {}", e))
        })?;

        let mut resp_line = String::new();
        let n = tokio::time::timeout(
            self.timeout,
            read_line_limited(&mut conn.stdout, &mut resp_line, MAX_MCP_RESPONSE_BYTES),
        )
        .await
        .map_err(|_| McpTransportError::Timeout(self.timeout))?
        .map_err(|e| McpTransportError::Connection(e.to_string()))?;

        if n == 0 {
            return Err(McpTransportError::Connection(
                "MCP server closed connection unexpectedly (server may have crashed)".into(),
            ));
        }

        let resp: JsonRpcResponse = serde_json::from_str(resp_line.trim()).map_err(|e| {
            McpTransportError::Connection(format!(
                "Failed to parse MCP response: {} (raw: {:?})",
                e,
                resp_line.trim()
            ))
        })?;

        if let Some(ref err) = resp.error {
            return Err(McpTransportError::Protocol {
                code: err.code,
                message: err.message.clone(),
            });
        }

        Ok(resp)
    }

    async fn send_notification(&self, req: &JsonRpcRequest) -> Result<(), McpTransportError> {
        let mut conn = self.conn.lock().await;

        let mut line = serde_json::to_string(req).map_err(|e| {
            McpTransportError::Connection(format!("Failed to serialize notification: {}", e))
        })?;
        line.push('\n');

        conn.stdin.write_all(line.as_bytes()).await.map_err(|e| {
            McpTransportError::Connection(format!("Failed to write notification: {}", e))
        })?;
        conn.stdin.flush().await.map_err(|e| {
            McpTransportError::Connection(format!("Failed to flush notification: {}", e))
        })?;

        Ok(())
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        let mut child = self.child.lock().await;

        #[cfg(unix)]
        {
            // Try SIGTERM first for graceful shutdown.
            if let Some(pid) = child.id() {
                unsafe {
                    if libc::kill(pid as i32, libc::SIGTERM) != 0 {
                        tracing::warn!(
                            pid,
                            errno = std::io::Error::last_os_error().raw_os_error(),
                            "SIGTERM send failed"
                        );
                    }
                }
            }

            // Wait up to 5 seconds for graceful exit.
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(_) => return Ok(()),
                Err(_) => {
                    // Timed out — force kill.
                    let _ = child.kill().await;
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = child.kill().await;
        }

        Ok(())
    }

    fn transport_name(&self) -> &str {
        &self.name
    }
}

/// Factory for creating fresh `StdioTransport` instances on reconnect.
///
/// Captures the spawn parameters so the supervisor can create a new child
/// process when the previous one crashes.
pub struct StdioTransportFactory {
    name: String,
    command: String,
    args: Vec<String>,
    extra_env: HashMap<String, String>,
    working_dir: Option<PathBuf>,
    timeout_secs: Option<u64>,
}

impl StdioTransportFactory {
    pub fn new(
        name: String,
        command: String,
        args: Vec<String>,
        extra_env: HashMap<String, String>,
        working_dir: Option<PathBuf>,
        timeout_secs: Option<u64>,
    ) -> Self {
        Self {
            name,
            command,
            args,
            extra_env,
            working_dir,
            timeout_secs,
        }
    }
}

#[async_trait]
impl McpTransportFactory for StdioTransportFactory {
    async fn create(&self) -> Result<Arc<dyn McpTransport>, McpTransportError> {
        let t = StdioTransport::spawn(
            self.name.clone(),
            self.command.clone(),
            self.args.clone(),
            self.extra_env.clone(),
            self.working_dir.clone(),
            self.timeout_secs,
        )
        .await?;
        Ok(Arc::new(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_nonexistent_command_returns_connection_error() {
        let result = StdioTransport::spawn(
            "test".into(),
            "nonexistent-binary-that-does-not-exist-xyz".into(),
            vec![],
            HashMap::new(),
            None,
            Some(5),
        )
        .await;
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.should_reconnect()); // Connection error
        assert!(err.to_string().contains("Failed to spawn"));
    }

    #[tokio::test]
    async fn transport_name_includes_server_name() {
        let transport = StdioTransport::spawn(
            "stdio:test-server".into(),
            "cat".into(),
            vec![],
            HashMap::new(),
            None,
            Some(2),
        )
        .await;
        // `cat` should spawn successfully
        if let Ok(t) = transport {
            assert_eq!(t.transport_name(), "stdio:test-server");
            let _ = t.close().await;
        }
    }
}
