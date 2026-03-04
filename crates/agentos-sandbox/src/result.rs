use serde::{Deserialize, Serialize};

/// Result of a sandboxed tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxResult {
    /// Captured stdout from the child process.
    pub stdout: String,
    /// Captured stderr from the child process.
    pub stderr: String,
    /// Exit code of the child process (0 = success).
    pub exit_code: i32,
    /// Total wall-clock execution time in milliseconds.
    pub wall_time_ms: u64,
    /// Whether the child was killed due to timeout.
    pub was_killed: bool,
}

impl SandboxResult {
    /// Check if the child process exited successfully.
    pub fn is_success(&self) -> bool {
        self.exit_code == 0 && !self.was_killed
    }
}
