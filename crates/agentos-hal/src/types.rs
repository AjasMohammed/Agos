use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub total_space_bytes: u64,
    pub available_space_bytes: u64,
    pub file_system: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    pub cpu_usage_percent: f32,
    pub cpu_core_count: usize,
    pub memory_total_mb: u64,
    pub memory_used_mb: u64,
    pub memory_available_mb: u64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    pub uptime_seconds: u64,
    pub os_name: String,
    pub os_version: String,
    pub hostname: String,
    pub load_average: (f64, f64, f64),
    pub disk_usage: Vec<DiskInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub pid: u32,
    pub name: String,
    pub cpu_usage_percent: f32,
    pub memory_mb: u64,
    pub status: String,
    pub parent_pid: Option<u32>,
    pub start_time: DateTime<Utc>,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketEntry {
    pub protocol: String,
    pub ip_version: String,
    pub local_addr: String,
    pub remote_addr: String,
    pub state: String,
    pub inode: u64,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketsResult {
    pub sockets: Vec<SocketEntry>,
    pub total_matched: usize,
    pub returned: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountEntry {
    pub device: String,
    pub mount_point: String,
    pub fs_type: String,
    pub options: String,
    pub writable: bool,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub use_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountsResult {
    pub mounts: Vec<MountEntry>,
    pub total_matched: usize,
    pub returned: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFileEntry {
    pub pid: u32,
    pub process_name: Option<String>,
    pub fd: i32,
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFilesResult {
    pub files: Vec<OpenFileEntry>,
    pub total_matched: usize,
    pub returned: usize,
    /// True when the walk was cut short at `limit` and `total_matched` reflects
    /// only the rows scanned so far. Pass `accurate_total: true` in params to
    /// force a full walk (slower).
    #[serde(default)]
    pub incomplete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitEntry {
    pub name: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitsResult {
    pub units: Vec<UnitEntry>,
    pub total_matched: usize,
    pub returned: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitStatus {
    pub name: String,
    pub active_state: String,
    pub sub_state: String,
    pub main_pid: Option<u32>,
    pub memory_current_bytes: Option<u64>,
    pub active_enter_timestamp: Option<DateTime<Utc>>,
    pub n_restarts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub timestamp: DateTime<Utc>,
    pub priority: u32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalResult {
    pub name: String,
    pub entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub name: String,
    pub ip_addresses: Vec<String>,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub packets_received: u64,
    pub packets_sent: u64,
    pub errors_in: u64,
    pub errors_out: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogSource {
    AppLog(String),
    SystemLog,
    KernelLog,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogQuery {
    pub source: LogSource,
    pub last_n_lines: Option<u64>,
    pub since: Option<DateTime<Utc>>,
    pub grep_pattern: Option<String>,
    pub level_filter: Option<Vec<LogLevel>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    #[serde(alias = "error")]
    Error,
    #[serde(alias = "warn", alias = "warning", alias = "Warning")]
    Warn,
    #[serde(alias = "info")]
    Info,
    #[serde(alias = "debug")]
    Debug,
    #[serde(alias = "trace")]
    Trace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
    pub source: String,
}
