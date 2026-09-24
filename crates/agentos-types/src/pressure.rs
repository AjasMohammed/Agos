//! Process-global resource pressure level.
//!
//! Set by the kernel's resource guard, read by anything that writes into the
//! data dir. It lives here as a static so `agentos-tools` and `agentos-memory`
//! can consult it without a handle threaded through every execution context.
//!
//! The point is to stop *before* the filesystem does. A full disk previously
//! surfaced as `SQLITE_FULL` errors logged and ignored — writes failed silently
//! while the kernel reported itself healthy.
//!
//! ponytail: one process, one level; per-mount levels if the data dir ever spans mounts.

use std::sync::atomic::{AtomicU8, Ordering};

/// How much room is left on the data-dir partition.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum PressureLevel {
    Ok = 0,
    Warn = 1,
    Critical = 2,
}

impl PressureLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

/// Whether a write may be dropped to keep the system alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteClass {
    /// Audit log, checkpoints, vault, kernel state, scheduler rows.
    /// Never paused: losing these breaks integrity or recovery, which is worse
    /// than running out of disk.
    Essential,
    /// Snapshots, HAL captures, episodic memory, derived artifacts.
    /// Paused under `Critical` — they are regenerable or expendable.
    Deferrable,
}

static LEVEL: AtomicU8 = AtomicU8::new(0);

fn from_u8(v: u8) -> PressureLevel {
    match v {
        0 => PressureLevel::Ok,
        1 => PressureLevel::Warn,
        _ => PressureLevel::Critical,
    }
}

pub fn level() -> PressureLevel {
    from_u8(LEVEL.load(Ordering::Relaxed))
}

/// Publish a new level, returning the previous one so callers can act on edges
/// (notify the operator once per transition, not once per tick).
pub fn set_level(new: PressureLevel) -> PressureLevel {
    from_u8(LEVEL.swap(new as u8, Ordering::Relaxed))
}

/// The gate every writer should consult.
pub fn writes_allowed(class: WriteClass) -> bool {
    match class {
        WriteClass::Essential => true,
        WriteClass::Deferrable => level() < PressureLevel::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test, not three: these all mutate the same process-global level, so
    // splitting them would need `serial_test` as a new dependency of this
    // crate just to avoid interleaving. Not worth a dep.
    #[test]
    fn pressure_gating() {
        set_level(PressureLevel::Ok);

        // Warn is advisory — deferrable writers keep going.
        assert_eq!(set_level(PressureLevel::Warn), PressureLevel::Ok);
        assert!(writes_allowed(WriteClass::Deferrable));
        assert!(writes_allowed(WriteClass::Essential));

        // Critical pauses deferrable writers but never essential ones:
        // dropping audit or checkpoint writes is worse than filling the disk.
        assert_eq!(set_level(PressureLevel::Critical), PressureLevel::Warn);
        assert!(!writes_allowed(WriteClass::Deferrable));
        assert!(writes_allowed(WriteClass::Essential));

        // set_level reports the previous value so callers fire once per edge.
        assert_eq!(set_level(PressureLevel::Ok), PressureLevel::Critical);
        assert!(writes_allowed(WriteClass::Deferrable));
    }
}
