//! Runtime disk-headroom guard.
//!
//! `[preflight]` checks headroom once at boot; this watches it continuously.
//! Without it a filling disk is invisible until SQLite starts returning
//! `database or disk is full` — writes fail, the errors scroll past in a log,
//! and `/readyz` still says 200.
//!
//! The guard publishes a process-global [`PressureLevel`]; deferrable writers
//! consult it through [`agentos_types::pressure::writes_allowed`].

use crate::config::ResourceGuardConfig;
use agentos_types::{pressure, PressureLevel};

/// A point-in-time measurement of the partition backing the data dir.
#[derive(Debug, Clone, Copy)]
pub struct DiskHeadroom {
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub free_inodes: u64,
    pub total_inodes: u64,
}

impl DiskHeadroom {
    pub fn free_mb(&self) -> u64 {
        self.free_bytes / (1024 * 1024)
    }
}

/// `statvfs` the first existing ancestor of `path`.
///
/// Cheap enough (microseconds, local syscall) to call inline on the supervisor
/// tick without `spawn_blocking`.
pub fn measure(path: &std::path::Path) -> anyhow::Result<DiskHeadroom> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::mem::MaybeUninit;
        use std::os::unix::ffi::OsStrExt;

        // Walk up to the first existing ancestor — the data dir may not exist
        // yet on a first boot.
        let mut check = path.to_path_buf();
        loop {
            if check.exists() {
                break;
            }
            match check.parent().map(|p| p.to_path_buf()) {
                Some(parent) if parent != check => check = parent,
                _ => {
                    check = std::path::PathBuf::from("/");
                    break;
                }
            }
        }

        let c_path = CString::new(check.as_os_str().as_bytes())
            .map_err(|e| anyhow::anyhow!("Invalid path for statvfs: {}", e))?;
        let mut stat = MaybeUninit::<libc::statvfs>::uninit();
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if ret != 0 {
            return Err(anyhow::anyhow!(
                "statvfs({}) failed: {}",
                check.display(),
                std::io::Error::last_os_error()
            ));
        }
        let stat = unsafe { stat.assume_init() };

        // f_bavail: blocks available to unprivileged processes (not f_bfree,
        // which includes the root reserve). Explicit u64 casts are defensive:
        // on 32-bit these types are u32 and multiplying before widening would
        // overflow.
        #[allow(clippy::unnecessary_cast)]
        let free_bytes = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
        #[allow(clippy::unnecessary_cast)]
        let total_bytes = (stat.f_blocks as u64).saturating_mul(stat.f_frsize as u64);
        #[allow(clippy::unnecessary_cast)]
        let free_inodes = stat.f_favail as u64;
        #[allow(clippy::unnecessary_cast)]
        let total_inodes = stat.f_files as u64;

        Ok(DiskHeadroom {
            free_bytes,
            total_bytes,
            free_inodes,
            total_inodes,
        })
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Err(anyhow::anyhow!(
            "Disk headroom measurement is only implemented on Unix"
        ))
    }
}

/// Map a measurement onto a pressure level.
///
/// Inodes matter independently of bytes: a partition with free space but no
/// free inodes fails writes just as hard.
pub fn classify(h: &DiskHeadroom, cfg: &ResourceGuardConfig) -> PressureLevel {
    let free_mb = h.free_mb();
    if free_mb < cfg.critical_free_mb || h.free_inodes < cfg.critical_free_inodes {
        PressureLevel::Critical
    } else if free_mb < cfg.warn_free_mb {
        PressureLevel::Warn
    } else {
        PressureLevel::Ok
    }
}

/// One guard cycle: measure, publish gauges, set the global level.
///
/// Returns `(previous, current, headroom)` so the caller can act on the edge
/// rather than on every tick.
pub fn tick(
    data_dir: &std::path::Path,
    cfg: &ResourceGuardConfig,
) -> anyhow::Result<(PressureLevel, PressureLevel, DiskHeadroom)> {
    let h = measure(data_dir)?;
    let now = classify(&h, cfg);
    crate::metrics::record_resource_headroom(h.free_bytes, h.free_inodes, now);
    let prev = pressure::set_level(now);
    if prev != now {
        tracing::warn!(
            from = prev.as_str(),
            to = now.as_str(),
            free_mb = h.free_mb(),
            free_inodes = h.free_inodes,
            "Resource pressure level changed"
        );
    }
    Ok((prev, now, h))
}

impl crate::kernel::Kernel {
    /// Called once per guard tick with the level edge.
    ///
    /// Phase 03 turns this into operator notification, audit events and a
    /// forced retention sweep. It is a no-op for now so the guard can land
    /// and start publishing metrics on its own.
    pub(crate) async fn on_pressure_changed(
        &self,
        _prev: PressureLevel,
        _now: PressureLevel,
        _headroom: DiskHeadroom,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ResourceGuardConfig {
        ResourceGuardConfig {
            enabled: true,
            check_interval_secs: 60,
            warn_free_mb: 2048,
            critical_free_mb: 512,
            critical_free_inodes: 10_000,
        }
    }

    fn headroom(free_mb: u64, free_inodes: u64) -> DiskHeadroom {
        DiskHeadroom {
            free_bytes: free_mb * 1024 * 1024,
            total_bytes: 100 * 1024 * 1024 * 1024,
            free_inodes,
            total_inodes: 1_000_000,
        }
    }

    #[test]
    fn classify_thresholds() {
        let c = cfg();
        assert_eq!(classify(&headroom(50_000, 900_000), &c), PressureLevel::Ok);
        assert_eq!(classify(&headroom(1_000, 900_000), &c), PressureLevel::Warn);
        assert_eq!(
            classify(&headroom(100, 900_000), &c),
            PressureLevel::Critical
        );
    }

    #[test]
    fn inode_exhaustion_is_critical_even_with_free_bytes() {
        // Plenty of space, no inodes: writes still fail, so this must be
        // Critical rather than Ok.
        assert_eq!(
            classify(&headroom(50_000, 5), &cfg()),
            PressureLevel::Critical
        );
    }

    #[test]
    fn measure_reports_a_real_partition() {
        let h = measure(std::path::Path::new(".")).expect("statvfs on cwd");
        assert!(h.total_bytes > 0, "total bytes should be non-zero");
        assert!(h.free_bytes <= h.total_bytes);
    }

    #[test]
    fn measure_walks_up_to_an_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does/not/exist/yet");
        let h = measure(&missing).expect("should fall back to an existing ancestor");
        assert!(h.total_bytes > 0);
    }
}
