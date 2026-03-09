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
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
    pub source: String,
}
