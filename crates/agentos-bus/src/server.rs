use crate::message::BusMessage;
use crate::transport::{read_message, write_message};
use agentos_types::AgentOSError;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

/// `(dev, ino)` of a path, or `None` if it does not exist.
///
/// Used as the socket's identity: unlinking by path alone lets a dying kernel
/// delete a *different* kernel's live socket that happens to sit at the same
/// path.
fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

pub struct BusServer {
    listener: UnixListener,
    socket_path: PathBuf,
    /// Identity of the socket file this server created; see [`socket_identity`].
    socket_id: Option<(u64, u64)>,
}

impl Drop for BusServer {
    fn drop(&mut self) {
        // Only unlink the socket if the file at our path is still the one we
        // bound. If another kernel replaced it, deleting it would break that
        // live instance (every CLI call would fail with "Is the kernel
        // running?" while it is still up).
        if self.socket_id.is_some() && socket_identity(&self.socket_path) == self.socket_id {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

impl BusServer {
    /// Start listening on the configured socket path.
    ///
    /// Refuses to start if another kernel is already listening there; only a
    /// genuinely stale socket file is removed.
    pub async fn bind(socket_path: &Path) -> Result<Self, AgentOSError> {
        if socket_path.exists() {
            // ponytail: a connect probe is the liveness test — no lockfile to
            // manage, no new dependency, and it asks the exact question that
            // matters (is someone serving this socket?).
            match UnixStream::connect(socket_path).await {
                Ok(_) => {
                    return Err(AgentOSError::BusError(format!(
                        "Another AgentOS kernel is already listening on {:?}. \
                         Refusing to start a second kernel: both would write the same \
                         databases (kernel_state.db, checkpoints.db, the audit log and the \
                         memory tiers) and corrupt them. You most likely double-started the \
                         kernel — stop the running one first (`systemctl --user stop agentos`, \
                         or `agentos status` to confirm what is up). If you are certain no \
                         kernel is running, delete the socket file and retry.",
                        socket_path
                    )));
                }
                // Nobody is listening: a leftover file from a crashed kernel.
                Err(e)
                    if e.kind() == ErrorKind::ConnectionRefused
                        || e.kind() == ErrorKind::NotFound =>
                {
                    tracing::warn!("Removing stale bus socket at {:?}", socket_path);
                    std::fs::remove_file(socket_path).map_err(|e| {
                        AgentOSError::BusError(format!("Failed to remove stale socket: {}", e))
                    })?;
                }
                // Anything else (e.g. permission denied) is not proof the socket
                // is dead — do not clobber it.
                Err(e) => {
                    return Err(AgentOSError::BusError(format!(
                        "Cannot determine whether a kernel owns the socket at {:?}: {}. \
                         Refusing to bind over it.",
                        socket_path, e
                    )));
                }
            }
        }

        let listener = UnixListener::bind(socket_path)
            .map_err(|e| AgentOSError::BusError(format!("Failed to bind to Unix socket: {}", e)))?;

        // Restrict socket to owner-only access (0600) to prevent other local users
        // from connecting and issuing privileged commands.
        #[cfg(unix)]
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |e| AgentOSError::BusError(format!("Failed to set socket permissions to 0600: {}", e)),
        )?;

        tracing::info!("Intent Bus listening on {:?}", socket_path);

        Ok(Self {
            listener,
            socket_path: socket_path.to_path_buf(),
            socket_id: socket_identity(socket_path),
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
