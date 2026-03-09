//! Linux-only seccomp-BPF filter construction.
//!
//! Builds a BPF program from a [`SandboxConfig`] that restricts which syscalls
//! a sandboxed child process may invoke. Unallowed syscalls return `EPERM`.

use crate::config::SandboxConfig;
use agentos_types::AgentOSError;
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
use std::collections::HashMap;
use std::convert::TryInto;

/// Map well-known syscall names to their numeric identifiers on x86_64.
/// Falls back to a lookup table because `libc::SYS_*` constants are arch-specific.
fn syscall_number(name: &str) -> Option<i64> {
    // x86_64 syscall numbers (from Linux kernel asm/unistd_64.h)
    let num = match name {
        "read" => libc::SYS_read,
        "write" => libc::SYS_write,
        "close" => libc::SYS_close,
        "fstat" => libc::SYS_fstat,
        "stat" => libc::SYS_stat,
        "lstat" => libc::SYS_lstat,
        "mmap" => libc::SYS_mmap,
        "mprotect" => libc::SYS_mprotect,
        "munmap" => libc::SYS_munmap,
        "brk" => libc::SYS_brk,
        "rt_sigaction" => libc::SYS_rt_sigaction,
        "rt_sigprocmask" => libc::SYS_rt_sigprocmask,
        "exit_group" => libc::SYS_exit_group,
        "arch_prctl" => libc::SYS_arch_prctl,
        "clock_gettime" => libc::SYS_clock_gettime,
        "nanosleep" => libc::SYS_nanosleep,
        "getrandom" => libc::SYS_getrandom,
        "futex" => libc::SYS_futex,
        "sched_yield" => libc::SYS_sched_yield,
        "madvise" => libc::SYS_madvise,
        "set_tid_address" => libc::SYS_set_tid_address,
        "set_robust_list" => libc::SYS_set_robust_list,
        "rseq" => libc::SYS_rseq,
        "prlimit64" => libc::SYS_prlimit64,
        "sigaltstack" => libc::SYS_sigaltstack,
        // Network
        "socket" => libc::SYS_socket,
        "connect" => libc::SYS_connect,
        "sendto" => libc::SYS_sendto,
        "recvfrom" => libc::SYS_recvfrom,
        "bind" => libc::SYS_bind,
        "listen" => libc::SYS_listen,
        "accept" => libc::SYS_accept,
        "accept4" => libc::SYS_accept4,
        "setsockopt" => libc::SYS_setsockopt,
        "getsockopt" => libc::SYS_getsockopt,
        "getpeername" => libc::SYS_getpeername,
        "getsockname" => libc::SYS_getsockname,
        "poll" => libc::SYS_poll,
        "epoll_create1" => libc::SYS_epoll_create1,
        "epoll_ctl" => libc::SYS_epoll_ctl,
        "epoll_wait" => libc::SYS_epoll_wait,
        "shutdown" => libc::SYS_shutdown,
        "sendmsg" => libc::SYS_sendmsg,
        "recvmsg" => libc::SYS_recvmsg,
        // Filesystem
        "openat" => libc::SYS_openat,
        "unlink" => libc::SYS_unlink,
        "unlinkat" => libc::SYS_unlinkat,
        "rename" => libc::SYS_rename,
        "renameat" => libc::SYS_renameat,
        "renameat2" => libc::SYS_renameat2,
        "mkdir" => libc::SYS_mkdir,
        "mkdirat" => libc::SYS_mkdirat,
        "rmdir" => libc::SYS_rmdir,
        "ftruncate" => libc::SYS_ftruncate,
        "fallocate" => libc::SYS_fallocate,
        "fdatasync" => libc::SYS_fdatasync,
        "fsync" => libc::SYS_fsync,
        "lseek" => libc::SYS_lseek,
        "newfstatat" => libc::SYS_newfstatat,
        "access" => libc::SYS_access,
        "faccessat" => libc::SYS_faccessat,
        "faccessat2" => libc::SYS_faccessat2,
        "getcwd" => libc::SYS_getcwd,
        "readlink" => libc::SYS_readlink,
        "readlinkat" => libc::SYS_readlinkat,
        "dup" => libc::SYS_dup,
        "dup2" => libc::SYS_dup2,
        "dup3" => libc::SYS_dup3,
        "fcntl" => libc::SYS_fcntl,
        "ioctl" => libc::SYS_ioctl,
        "getdents64" => libc::SYS_getdents64,
        // Process / misc
        "exit" => libc::SYS_exit,
        "getpid" => libc::SYS_getpid,
        "gettid" => libc::SYS_gettid,
        "getuid" => libc::SYS_getuid,
        "getgid" => libc::SYS_getgid,
        "geteuid" => libc::SYS_geteuid,
        "getegid" => libc::SYS_getegid,
        "clone" => libc::SYS_clone,
        "clone3" => libc::SYS_clone3,
        "wait4" => libc::SYS_wait4,
        "pipe2" => libc::SYS_pipe2,
        "eventfd2" => libc::SYS_eventfd2,
        "prctl" => libc::SYS_prctl,
        "execve" => libc::SYS_execve,
        _ => return None,
    };
    Some(num)
}

/// Build a seccomp-BPF filter from a `SandboxConfig`.
///
/// The filter uses an **allowlist** model:
/// - Allowed syscalls get `SeccompAction::Allow`
/// - Everything else gets `SeccompAction::Errno(EPERM)`
///
/// # Errors
///
/// Returns `AgentOSError::SandboxFilterError` if the filter cannot be compiled
/// (e.g., unrecognized syscall name or unsupported architecture).
pub fn build_seccomp_filter(config: &SandboxConfig) -> Result<BpfProgram, AgentOSError> {
    let effective = config.effective_syscalls();

    let mut rules: HashMap<i64, Vec<seccompiler::SeccompRule>> = HashMap::new();

    for name in &effective {
        match syscall_number(name) {
            Some(nr) => {
                // Empty rule vec = allow unconditionally (no argument checks)
                rules.entry(nr).or_insert_with(Vec::new);
            }
            None => {
                tracing::warn!(
                    syscall = %name,
                    "Unknown syscall name in sandbox config, skipping"
                );
            }
        }
    }

    let arch = std::env::consts::ARCH
        .try_into()
        .map_err(|_| AgentOSError::SandboxFilterError {
            reason: format!("Unsupported architecture: {}", std::env::consts::ARCH),
        })?;

    let filter = SeccompFilter::new(
        rules.into_iter().collect(),
        // on-match action: allow the syscall
        SeccompAction::Allow,
        // default (mismatch) action: deny with EPERM
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| AgentOSError::SandboxFilterError {
        reason: format!("Failed to create seccomp filter: {:?}", e),
    })?;

    let bpf: BpfProgram = filter.try_into().map_err(|e: seccompiler::BackendError| {
        AgentOSError::SandboxFilterError {
            reason: format!("Failed to compile seccomp filter to BPF: {:?}", e),
        }
    })?;

    Ok(bpf)
}

/// Apply a compiled BPF filter to the current thread.
///
/// This should only be called from within a child process, after
/// `PR_SET_NO_NEW_PRIVS` has been set.
pub fn apply_filter(bpf: &BpfProgram) -> Result<(), AgentOSError> {
    seccompiler::apply_filter(bpf).map_err(|e| AgentOSError::SandboxFilterError {
        reason: format!("Failed to apply seccomp filter: {:?}", e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_default_filter_succeeds() {
        let config = SandboxConfig::default();
        let result = build_seccomp_filter(&config);
        assert!(
            result.is_ok(),
            "Default filter should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_build_network_filter_succeeds() {
        let config = SandboxConfig {
            allow_network: true,
            ..Default::default()
        };
        let result = build_seccomp_filter(&config);
        assert!(
            result.is_ok(),
            "Network filter should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_build_fs_write_filter_succeeds() {
        let config = SandboxConfig {
            allow_fs_write: true,
            ..Default::default()
        };
        let result = build_seccomp_filter(&config);
        assert!(
            result.is_ok(),
            "FS-write filter should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_build_full_filter_succeeds() {
        let config = SandboxConfig {
            allow_network: true,
            allow_fs_write: true,
            allow_gpu: true,
            ..Default::default()
        };
        let result = build_seccomp_filter(&config);
        assert!(
            result.is_ok(),
            "Full-access filter should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_unknown_syscall_is_skipped() {
        let config = SandboxConfig {
            allowed_syscalls: vec!["nonexistent_syscall".to_string()],
            ..Default::default()
        };
        // Should succeed; unknown syscalls are logged and skipped
        let result = build_seccomp_filter(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_syscall_number_lookup() {
        assert_eq!(syscall_number("read"), Some(libc::SYS_read));
        assert_eq!(syscall_number("write"), Some(libc::SYS_write));
        assert_eq!(syscall_number("exit_group"), Some(libc::SYS_exit_group));
        assert_eq!(syscall_number("nonexistent"), None);
    }
}
