---
title: Boot Pre-flight Checks
tags:
  - kernel
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 4h
priority: high
---

# Boot Pre-flight Checks

> Add system health validation at the start of `Kernel::boot()` to prevent crash loops from degraded system conditions (low disk, corrupt DB, inaccessible paths).

---

## Why This Subtask

The kernel's 4 unclean restarts in 50 minutes were likely caused by cascading failures: disk pressure causes SQLite WAL checkpoint failures, which crash subsystems, triggering restarts into the same degraded state. Currently `Kernel::boot()` initializes subsystems sequentially (config -> audit -> vault -> tools -> memory -> bus) without validating whether the system can support them.

Pre-flight checks at the top of `boot()` catch these problems early and return clear errors instead of deep subsystem panics.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Disk space check | None until health_monitor starts (30s+ after boot) | Pre-flight check before any subsystem init |
| DB writability | Discovered when `AuditLog::open()` or `SecretsVault::open()` fails | Explicit write test to parent directories |
| Minimum free space | Not configurable | `preflight.min_free_disk_mb` config key (default: 100) |
| Error reporting | Deep subsystem error (e.g., "WAL checkpoint failed") | Clear pre-flight message: "Insufficient disk space: 45MB free, 100MB required" |

## What to Do

1. Open `crates/agentos-kernel/src/config.rs`

2. Add a `PreflightConfig` struct with defaults:

```rust
/// Configuration for boot-time pre-flight system health checks.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PreflightConfig {
    /// Minimum free disk space in MB on the data directory partition.
    /// Boot fails if free space is below this threshold.
    #[serde(default = "default_min_free_disk_mb")]
    pub min_free_disk_mb: u64,
    /// Whether to perform a write test on database directories.
    #[serde(default = "default_check_db_writable")]
    pub check_db_writable: bool,
}

impl Default for PreflightConfig {
    fn default() -> Self {
        Self {
            min_free_disk_mb: default_min_free_disk_mb(),
            check_db_writable: default_check_db_writable(),
        }
    }
}

fn default_min_free_disk_mb() -> u64 {
    100
}

fn default_check_db_writable() -> bool {
    true
}
```

3. Add `preflight` field to `KernelConfig`:

```rust
pub struct KernelConfig {
    // ... existing fields ...
    #[serde(default)]
    pub preflight: PreflightConfig,
}
```

4. Open `crates/agentos-kernel/src/kernel.rs`

5. Add a `preflight_checks()` function (as a free function or associated function on `Kernel`):

```rust
/// Run pre-flight system health checks before initializing subsystems.
/// These checks validate that the system can support kernel operation.
/// Returns Err with a descriptive message if any check fails.
fn preflight_checks(config: &KernelConfig) -> Result<(), anyhow::Error> {
    let data_dir = std::path::Path::new(&config.tools.data_dir);

    // 1. Check disk space on the data directory partition
    if config.preflight.min_free_disk_mb > 0 {
        let free_mb = get_free_disk_mb(data_dir)?;
        if free_mb < config.preflight.min_free_disk_mb {
            return Err(anyhow::anyhow!(
                "Pre-flight check failed: insufficient disk space on {}. \
                 Free: {} MB, required: {} MB. \
                 Free disk space or reduce preflight.min_free_disk_mb in config.",
                data_dir.display(),
                free_mb,
                config.preflight.min_free_disk_mb,
            ));
        }
        tracing::info!(
            free_mb,
            min_required_mb = config.preflight.min_free_disk_mb,
            "Pre-flight: disk space OK"
        );
    }

    // 2. Check that database directories are writable
    if config.preflight.check_db_writable {
        for (name, path_str) in &[
            ("audit", config.audit.log_path.as_str()),
            ("vault", config.secrets.vault_path.as_str()),
        ] {
            let path = std::path::Path::new(path_str);
            if let Some(parent) = path.parent() {
                if parent.exists() {
                    // Test write access by creating and removing a temp file
                    let test_file = parent.join(".agentos_preflight_test");
                    match std::fs::write(&test_file, b"preflight") {
                        Ok(()) => {
                            let _ = std::fs::remove_file(&test_file);
                            tracing::info!(path = %parent.display(), "{} directory writable", name);
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "Pre-flight check failed: {} directory {} is not writable: {}",
                                name,
                                parent.display(),
                                e,
                            ));
                        }
                    }
                }
                // If parent doesn't exist yet, boot() will create it -- this is OK
            }
        }
    }

    Ok(())
}

/// Get free disk space in MB for the partition containing the given path.
fn get_free_disk_mb(path: &std::path::Path) -> Result<u64, anyhow::Error> {
    // Use the path itself or its first existing ancestor
    let check_path = if path.exists() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent() {
        if parent.exists() {
            parent.to_path_buf()
        } else {
            // Fall back to root
            std::path::PathBuf::from("/")
        }
    } else {
        std::path::PathBuf::from("/")
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let stat = nix::sys::statvfs::statvfs(&check_path)
            .map_err(|e| anyhow::anyhow!("statvfs({}) failed: {}", check_path.display(), e))?;
        let free_bytes = stat.blocks_available() as u64 * stat.fragment_size() as u64;
        Ok(free_bytes / (1024 * 1024))
    }

    #[cfg(not(unix))]
    {
        // On non-Unix platforms, skip the disk check
        tracing::warn!("Disk space pre-flight check not available on this platform");
        Ok(u64::MAX)
    }
}
```

Note: This requires adding `nix` as a dependency to `agentos-kernel/Cargo.toml` for `statvfs`. Alternatively, use `fs2` crate's `free_space()` or read from `/proc/mounts` directly. Check if `nix` is already a dependency:

```toml
# In crates/agentos-kernel/Cargo.toml, add:
[target.'cfg(unix)'.dependencies]
nix = { version = "0.29", features = ["fs"] }
```

If you prefer to avoid a new dependency, use `std::process::Command` to call `df`:

```rust
#[cfg(unix)]
fn get_free_disk_mb(path: &std::path::Path) -> Result<u64, anyhow::Error> {
    let output = std::process::Command::new("df")
        .args(["--output=avail", "-BM"])
        .arg(path)
        .output()
        .map_err(|e| anyhow::anyhow!("df command failed: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse second line (first is header), strip trailing 'M'
    let avail_str = stdout.lines().nth(1)
        .ok_or_else(|| anyhow::anyhow!("Unexpected df output"))?
        .trim()
        .trim_end_matches('M');
    avail_str.parse::<u64>()
        .map_err(|e| anyhow::anyhow!("Failed to parse df output '{}': {}", avail_str, e))
}
```

6. Call `preflight_checks()` at the top of `Kernel::boot()`, after loading config (line 123) but before creating directories (line 134):

```rust
pub async fn boot(
    config_path: &Path,
    vault_passphrase: &ZeroizingString,
) -> Result<Self, anyhow::Error> {
    let config = load_config(config_path)?;
    // ... tracing::info for config loaded ...

    // Run pre-flight system health checks
    preflight_checks(&config)?;

    // Ensure directories exist ...
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/config.rs` | Add `PreflightConfig` struct and field on `KernelConfig` |
| `crates/agentos-kernel/src/kernel.rs` | Add `preflight_checks()` and `get_free_disk_mb()` functions; call from `boot()` |

## Prerequisites

None -- this subtask is independent.

## Test Plan

- `cargo test -p agentos-kernel -- preflight` passes
- Add unit test: `preflight_checks()` with a config where `min_free_disk_mb = 0` (disabled) succeeds
- Add unit test: `get_free_disk_mb("/")` returns a value > 0 on Linux
- Add unit test: `preflight_checks()` with a non-writable directory returns an error (use `tempfile` with read-only permissions)

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- preflight --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
