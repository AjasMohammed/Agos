//! Landlock LSM — unprivileged filesystem write restriction.
//!
//! Restricts all filesystem write operations in the calling process to a
//! single allowed path (the tool's data_dir). Read access is unrestricted so
//! the child can still load shared libraries and read its request file.
//!
//! Designed to be called from a `pre_exec` hook (between fork and exec), so
//! it uses only raw `libc::syscall()` invocations — no heap allocation, no
//! Rust runtime, fully async-signal-safe.
//!
//! Gracefully degrades to a no-op on kernels older than 5.13 (ENOSYS) or when
//! the Landlock ABI is disabled. Returns an error only if the kernel reports
//! Landlock as available but we fail to apply the ruleset.

// Landlock syscall numbers (x86_64, from linux/unistd_64.h since Linux 5.13).
// These values are architecture-independent — Landlock was added post-abi-unification.
const SYS_LANDLOCK_CREATE_RULESET: i64 = 444;
const SYS_LANDLOCK_ADD_RULE: i64 = 445;
const SYS_LANDLOCK_RESTRICT_SELF: i64 = 446;

// landlock_create_ruleset flags
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;

// Rule type for path-based restrictions
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

// Filesystem access rights — write-class only (from linux/landlock.h).
// We do NOT restrict read or execute, so the child can read system libraries,
// its request file, etc. Values match the kernel UAPI header exactly.
//
// ABI v1 (kernel 5.13+): bits 0-13
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 12;
const LANDLOCK_ACCESS_FS_MAKE_IPC: u64 = 1 << 13;
// ABI v2 (kernel 5.19+): cross-directory link/rename
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 14;
// ABI v3 (kernel 6.2+): file truncation
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 15;

/// All write-type access rights covered by Landlock ABI v1 (Linux 5.13+).
const WRITE_ACCESS_V1: u64 = LANDLOCK_ACCESS_FS_WRITE_FILE
    | LANDLOCK_ACCESS_FS_REMOVE_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_FILE
    | LANDLOCK_ACCESS_FS_MAKE_CHAR
    | LANDLOCK_ACCESS_FS_MAKE_DIR
    | LANDLOCK_ACCESS_FS_MAKE_REG
    | LANDLOCK_ACCESS_FS_MAKE_SYM
    | LANDLOCK_ACCESS_FS_MAKE_SOCK
    | LANDLOCK_ACCESS_FS_MAKE_FIFO
    | LANDLOCK_ACCESS_FS_MAKE_BLOCK
    | LANDLOCK_ACCESS_FS_MAKE_IPC;

/// Kernel ABI structs — must exactly match linux/landlock.h.
///
/// `LandlockRulesetAttr` is 8 bytes (one u64 field, no padding).
#[repr(C)]
struct LandlockRulesetAttr {
    /// Bitmask of FS access rights this ruleset restricts.
    handled_access_fs: u64,
}

/// `LandlockPathBeneathAttr` must be packed to match the kernel definition
/// (`__attribute__((packed))`): u64 at offset 0, i32 at offset 8 = 12 bytes total.
/// Without `packed`, Rust's `#[repr(C)]` would add 4 bytes of trailing padding
/// to align the struct to 8 bytes (total 16), mismatching the kernel's 12-byte layout.
#[repr(C, packed)]
struct LandlockPathBeneathAttr {
    /// Access rights allowed under this directory.
    allowed_access: u64,
    /// Open file descriptor of the allowed parent directory.
    parent_fd: i32,
}

// Compile-time assertions: catch ABI drift if struct definitions change.
const _: () = assert!(
    std::mem::size_of::<LandlockRulesetAttr>() == 8,
    "LandlockRulesetAttr must be 8 bytes"
);
const _: () = assert!(
    std::mem::size_of::<LandlockPathBeneathAttr>() == 12,
    "LandlockPathBeneathAttr must be 12 bytes (packed)"
);

/// Apply Landlock write restriction in a `pre_exec` (async-signal-safe) context.
///
/// After this call, only paths under `data_dir_bytes` (a null-terminated path)
/// are writable. The Landlock ruleset survives the subsequent `execve()` and
/// continues to restrict the spawned sandbox binary.
///
/// Write-type access rights restricted (all others unrestricted):
/// - v1 (5.13+): WRITE_FILE, REMOVE_DIR/FILE, MAKE_{CHAR,DIR,REG,SYM,SOCK,FIFO,BLOCK,IPC}
/// - v2 (5.19+): also REFER (cross-directory link/rename)
/// - v3 (6.2+): also TRUNCATE
///
/// # Safety
///
/// Must only be called from within a `pre_exec` hook (after fork, before exec).
/// Uses only raw libc syscalls — no allocation, no unwinding.
///
/// # Graceful degradation
///
/// Returns `Ok(())` without applying any restriction when the kernel does not
/// support Landlock (`ENOSYS` / `EOPNOTSUPP`, i.e. kernels older than 5.13).
///
/// Returns `Err` when Landlock is confirmed available but the ruleset fails to
/// apply — this includes when `data_dir_bytes` cannot be opened, since at that
/// point the sandbox has been created and leaving it partially applied is unsafe.
pub unsafe fn apply_write_restriction(data_dir_bytes: &[u8]) -> std::io::Result<()> {
    // Probe ABI version. Passing NULL + size=0 + LANDLOCK_CREATE_RULESET_VERSION
    // returns the highest supported ABI version, or a negative errno if Landlock
    // is unavailable (ENOSYS on old kernels, EOPNOTSUPP if disabled).
    let abi_version = libc::syscall(
        SYS_LANDLOCK_CREATE_RULESET,
        std::ptr::null::<LandlockRulesetAttr>(),
        0usize,
        LANDLOCK_CREATE_RULESET_VERSION,
    );
    if abi_version < 1 {
        // Kernel doesn't support Landlock — silently skip.
        return Ok(());
    }

    // Build the write access mask for the detected ABI version.
    // Each bit added must be known to the kernel (otherwise EINVAL).
    let write_access = WRITE_ACCESS_V1
        | if abi_version >= 2 {
            LANDLOCK_ACCESS_FS_REFER
        } else {
            0
        }
        | if abi_version >= 3 {
            LANDLOCK_ACCESS_FS_TRUNCATE
        } else {
            0
        };

    // --- Step 1: create ruleset ---
    let attr = LandlockRulesetAttr {
        handled_access_fs: write_access,
    };
    let ruleset_fd = libc::syscall(
        SYS_LANDLOCK_CREATE_RULESET,
        &attr as *const LandlockRulesetAttr,
        std::mem::size_of::<LandlockRulesetAttr>(),
        0u32, // flags = 0
    );
    if ruleset_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ruleset_fd = ruleset_fd as libc::c_int;

    // --- Step 2: open data_dir and add the allow-write rule ---
    // O_NOFOLLOW prevents a symlink-substitution attack where data_dir is
    // replaced with a symlink pointing to a privileged directory.
    let dir_fd = libc::open(
        data_dir_bytes.as_ptr() as *const libc::c_char,
        libc::O_PATH | libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    );
    if dir_fd < 0 {
        // Landlock is available but data_dir can't be opened. Fail hard:
        // leaving the ruleset partially applied (created but not restricted)
        // is safe (the process is unrestricted), but this indicates a
        // misconfiguration that the caller should know about.
        libc::close(ruleset_fd);
        return Err(std::io::Error::last_os_error());
    }

    // NOTE: fields of #[repr(C, packed)] structs must not be referenced —
    // only the whole struct can be referenced. We initialize via struct literal
    // and immediately pass a pointer to libc::syscall, so there are no
    // unaligned field references.
    let path_attr = LandlockPathBeneathAttr {
        allowed_access: write_access,
        parent_fd: dir_fd,
    };
    let ret = libc::syscall(
        SYS_LANDLOCK_ADD_RULE,
        ruleset_fd,
        LANDLOCK_RULE_PATH_BENEATH,
        &path_attr as *const LandlockPathBeneathAttr,
        0u32, // flags = 0
    );
    libc::close(dir_fd);
    if ret < 0 {
        libc::close(ruleset_fd);
        return Err(std::io::Error::last_os_error());
    }

    // --- Step 3: PR_SET_NO_NEW_PRIVS (required before restrict_self) ---
    // Idempotent — safe to call even if the executor sets it again later.
    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
        libc::close(ruleset_fd);
        return Err(std::io::Error::last_os_error());
    }

    // --- Step 4: restrict self ---
    let ret = libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32);
    libc::close(ruleset_fd);
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Probe the Landlock ABI version on this kernel without applying any
    /// restriction. Validates the syscall interface is reachable.
    #[test]
    fn test_landlock_abi_probe() {
        let abi_version = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                std::ptr::null::<LandlockRulesetAttr>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        };
        // abi_version >= 1 means Landlock is available; < 0 means it's not.
        // Both outcomes are valid — we just assert the value is plausible.
        assert!(abi_version >= -4096, "syscall returned implausible value");
    }

    /// Struct size assertions are compile-time, but this test documents the
    /// expected sizes to make ABI requirements visible in test output.
    #[test]
    fn test_struct_sizes_match_kernel_abi() {
        assert_eq!(std::mem::size_of::<LandlockRulesetAttr>(), 8);
        assert_eq!(std::mem::size_of::<LandlockPathBeneathAttr>(), 12);
    }

    #[test]
    fn test_write_restriction_with_nonexistent_dir_returns_err_when_landlock_available() {
        // When Landlock is available, failing to open data_dir returns an error
        // (the ruleset was created but couldn't be applied — caller must know).
        // When Landlock is unavailable the function returns Ok early (skips).
        let bad_path = b"/nonexistent/agentos/sandbox/test\0";
        let result = unsafe { apply_write_restriction(bad_path) };
        let abi_available = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                std::ptr::null::<LandlockRulesetAttr>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            ) >= 1
        };
        if abi_available {
            assert!(
                result.is_err(),
                "Should fail when Landlock available but data_dir missing"
            );
        } else {
            assert!(result.is_ok(), "Should skip gracefully on old kernel");
        }
    }

    // NOTE: We intentionally do NOT test `apply_write_restriction` with a real
    // existing directory in unit tests. Landlock restrictions are process-wide
    // and irreversible — applying them in the test runner process would
    // permanently restrict all subsequent tests to only write under the
    // (soon-to-be-deleted) TempDir, causing flaky EACCES failures.
    //
    // The function is exercised end-to-end by the sandbox integration tests
    // which run each tool in a forked child process.
}
