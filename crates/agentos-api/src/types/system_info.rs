//! DTOs for the observability & system-introspection surface:
//! doctor diagnostics, log queries, resource snapshots, and HAL devices.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

// ── Doctor ───────────────────────────────────────────────────────────────

/// Aggregate result of running all diagnostic checks.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    /// True when no check is in the `fail` state.
    pub all_ok: bool,
}

/// A single diagnostic check result.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DoctorCheck {
    /// Human-readable check name, e.g. `"Config file exists"`.
    pub name: String,
    /// One of `"pass"`, `"warn"`, `"fail"`.
    pub status: String,
    /// Detail message (and a suggested fix when applicable).
    pub detail: String,
    /// Whether `POST /api/v1/doctor/fix` can attempt to repair this check.
    pub fixable: bool,
}

/// Request body for `POST /api/v1/doctor/fix`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DoctorFixRequest {
    /// Name of the check to attempt to fix. Empty/omitted = fix all fixable.
    #[serde(default)]
    pub check: String,
}

// ── Logs ─────────────────────────────────────────────────────────────────

/// Query parameters for `GET /api/v1/logs`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
pub struct LogQuery {
    /// Severity filter (substring match, case-insensitive).
    pub level: Option<String>,
    /// RFC3339 lower bound on the entry timestamp.
    pub since: Option<String>,
    /// Maximum number of lines to return (default 200).
    pub limit: Option<u32>,
}

/// A single audit-log line.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LogLine {
    pub timestamp: String,
    pub severity: String,
    pub event_type: String,
    /// The raw JSONL line as written to the audit log.
    pub line: String,
}

// ── Resources ────────────────────────────────────────────────────────────

/// Host resource snapshot plus live resource-arbiter lock state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceInfo {
    pub data_dir: String,
    pub disk_free_bytes: u64,
    pub disk_total_bytes: u64,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub locks: Vec<ResourceLockInfo>,
    /// Contention statistics keyed by resource id.
    #[schema(value_type = Object)]
    pub contention: serde_json::Value,
}

/// A currently held resource lock.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceLockInfo {
    pub resource_id: String,
    /// `"exclusive"` or `"shared"`.
    pub lock_mode: String,
    /// Agent id(s) holding the lock.
    pub held_by: String,
    /// RFC3339 acquisition timestamp.
    pub acquired_at: String,
    pub ttl_seconds: u64,
    pub waiters: usize,
}

// ── HAL ──────────────────────────────────────────────────────────────────

/// Hardware abstraction layer device inventory plus a system snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HalInfo {
    pub devices: Vec<HalDevice>,
    /// Raw `SystemSnapshot` (cpu/mem/disk/load) as JSON.
    #[schema(value_type = Object)]
    pub system: serde_json::Value,
}

/// A registered HAL device and its per-agent access policy.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HalDevice {
    pub id: String,
    pub device_type: String,
    /// `"pending"`, `"approved"`, or `"quarantined"`.
    pub status: String,
    pub granted_to: Vec<String>,
    pub denied_to: Vec<String>,
}
