use serde::{Deserialize, Serialize};

/// Sandbox configuration derived from a tool manifest's `[sandbox]` section.
/// Controls what the child process is allowed to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Whether the tool is allowed to make network connections.
    pub allow_network: bool,
    /// Whether the tool is allowed to write to the filesystem.
    pub allow_fs_write: bool,
    /// Whether the tool is allowed GPU device access.
    pub allow_gpu: bool,
    /// Tool-declared memory budget from the manifest, in bytes.
    /// The final RLIMIT_AS also includes a per-category process overhead.
    pub max_memory_bytes: u64,
    /// Maximum CPU time in milliseconds (RLIMIT_CPU + wall-clock timeout).
    pub max_cpu_ms: u64,
    /// Explicit syscall allowlist. If empty, uses the default base allowlist.
    pub allowed_syscalls: Vec<String>,
}

impl SandboxConfig {
    /// Process overhead baseline for stateless tools.
    /// The CLI binary is dynamically linked, so shared-library mappings need
    /// more headroom than RSS alone would suggest.
    pub const OVERHEAD_STATELESS: u64 = 192 * 1024 * 1024;
    /// Process overhead baseline for tools that initialize embedder-backed memory stores.
    pub const OVERHEAD_MEMORY: u64 = 768 * 1024 * 1024;
    /// Process overhead baseline for HTTP/TLS tooling.
    pub const OVERHEAD_NETWORK: u64 = 256 * 1024 * 1024;
    /// Process overhead baseline for HAL-backed system introspection.
    pub const OVERHEAD_HAL: u64 = 192 * 1024 * 1024;
    /// Generous fallback when a tool category is unknown.
    pub const OVERHEAD_DEFAULT: u64 = 512 * 1024 * 1024;

    /// Compute the RLIMIT_AS value by combining the tool's declared budget with
    /// the baseline overhead needed to start a child process in this category.
    pub const fn rlimit_as_bytes(&self, category_overhead_bytes: u64) -> u64 {
        self.max_memory_bytes
            .saturating_add(category_overhead_bytes)
    }

    /// Derive a `SandboxConfig` from a tool manifest's sandbox section.
    pub fn from_manifest(sandbox: &agentos_types::ToolSandbox) -> Self {
        Self {
            allow_network: sandbox.network,
            allow_fs_write: sandbox.fs_write,
            allow_gpu: sandbox.gpu,
            max_memory_bytes: sandbox.max_memory_mb * 1024 * 1024,
            max_cpu_ms: sandbox.max_cpu_ms,
            allowed_syscalls: sandbox.syscalls.clone(),
        }
    }

    /// The default base syscall allowlist required for any Rust binary to function.
    /// These are the minimum syscalls needed for basic process lifecycle.
    pub fn default_base_syscalls() -> &'static [&'static str] {
        &[
            "read",
            "readv",
            "write",
            "writev",
            "close",
            "fstat",
            "mmap",
            "mprotect",
            "munmap",
            "mremap",
            "brk",
            "rt_sigaction",
            "rt_sigprocmask",
            "rt_sigreturn",
            "exit",
            "exit_group",
            "arch_prctl",
            "clock_gettime",
            "clock_getres",
            "nanosleep",
            "getrandom",
            "futex",
            "sched_yield",
            "sched_getaffinity",
            // Required for Rust allocator / runtime
            "madvise",
            "set_tid_address",
            "set_robust_list",
            "rseq",
            "prlimit64",
            "sigaltstack",
            // Required for Tokio async runtime (epoll event loop)
            "poll",
            "epoll_create1",
            "epoll_ctl",
            "epoll_wait",
            // Required for Tokio multi-threaded runtime thread spawning
            "clone3",
            "clone",
            // Required for process/thread lifecycle
            "wait4",
            "pipe2",
            "eventfd2",
            "prctl",
            // Required because seccomp is applied in pre_exec BEFORE exec().
            // Without this, the child cannot execve the sandbox binary.
            "execve",
            "getpid",
            "gettid",
            "getuid",
            "getgid",
            "geteuid",
            "getegid",
            // Required by glibc 2.33+ for stat operations
            "statx",
            // Required for Rust panic handler / libc abort() mechanism.
            // When a panic occurs, libc::abort() calls raise(SIGABRT) which
            // uses tgkill/tkill. Without these, abort falls back to ud2
            // (illegal instruction), killing the process with SIGILL.
            "tgkill",
            "tkill",
            "kill",
            // Required for glibc buffered I/O: pread64 is used by fread()
            // for positional reads on file descriptors.
            "pread64",
            "pwrite64",
            // Required for tokio signal driver (self-pipe trick).
            // enable_all() activates the signal driver which uses socketpair.
            "socketpair",
            // Required by Rust allocator / glibc to query system memory info.
            "sysinfo",
            // Required by glibc NSS modules (libnss_myhostname.so from systemd).
            // Any hostname/DNS resolution triggers NSS → dlopen(libnss_myhostname) →
            // gethostname_malloc() → uname(). Without this, the module hard-asserts
            // and aborts the child process.
            "uname",
        ]
    }

    /// Additional syscalls granted when `allow_network = true`.
    pub fn network_syscalls() -> &'static [&'static str] {
        &[
            "socket",
            "connect",
            "sendto",
            "recvfrom",
            "bind",
            "listen",
            "accept",
            "accept4",
            "setsockopt",
            "getsockopt",
            "getpeername",
            "getsockname",
            "poll",
            "shutdown",
            "sendmsg",
            "recvmsg",
        ]
    }

    /// Additional syscalls granted when `allow_fs_write = true`.
    pub fn fs_write_syscalls() -> &'static [&'static str] {
        &[
            "openat",
            "unlink",
            "unlinkat",
            "rename",
            "renameat",
            "renameat2",
            "mkdir",
            "mkdirat",
            "rmdir",
            "ftruncate",
            "fallocate",
            "fdatasync",
            "fsync",
            "lseek",
            "stat",
            "newfstatat",
            "access",
            "faccessat",
            "faccessat2",
            "getcwd",
            "readlink",
            "readlinkat",
            "dup",
            "dup2",
            "dup3",
            "fcntl",
            "ioctl",
        ]
    }

    /// Compute the full list of allowed syscall names for this config.
    pub fn effective_syscalls(&self) -> Vec<String> {
        let mut syscalls: Vec<String> = if self.allowed_syscalls.is_empty() {
            Self::default_base_syscalls()
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            // When custom syscalls are specified, merge with base set
            let mut set: Vec<String> = Self::default_base_syscalls()
                .iter()
                .map(|s| s.to_string())
                .collect();
            for s in &self.allowed_syscalls {
                if !set.contains(s) {
                    set.push(s.clone());
                }
            }
            set
        };

        if self.allow_network {
            for s in Self::network_syscalls() {
                let s = s.to_string();
                if !syscalls.contains(&s) {
                    syscalls.push(s);
                }
            }
        }

        if self.allow_fs_write {
            for s in Self::fs_write_syscalls() {
                let s = s.to_string();
                if !syscalls.contains(&s) {
                    syscalls.push(s);
                }
            }
        }

        // Always allow read-only fs operations — tools need to at least
        // read from their data_dir even if fs_write is false.
        let read_only_fs = [
            "openat",
            "stat",
            "newfstatat",
            "access",
            "faccessat",
            "faccessat2",
            "getcwd",
            "readlink",
            "readlinkat",
            "lseek",
            "dup",
            "dup2",
            "dup3",
            "fcntl",
            "ioctl",
            "getdents64",
        ];
        for s in read_only_fs {
            let s = s.to_string();
            if !syscalls.contains(&s) {
                syscalls.push(s);
            }
        }

        syscalls
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            allow_network: false,
            allow_fs_write: false,
            allow_gpu: false,
            max_memory_bytes: 64 * 1024 * 1024, // 64 MiB
            max_cpu_ms: 30_000,                 // 30 seconds
            allowed_syscalls: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::ToolSandbox;

    #[test]
    fn test_from_manifest_defaults() {
        let sandbox = ToolSandbox {
            network: false,
            fs_write: false,
            gpu: false,
            max_memory_mb: 64,
            max_cpu_ms: 5000,
            syscalls: Vec::new(),
            weight: None,
        };
        let config = SandboxConfig::from_manifest(&sandbox);
        assert!(!config.allow_network);
        assert!(!config.allow_fs_write);
        assert!(!config.allow_gpu);
        assert_eq!(config.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(config.max_cpu_ms, 5000);
        assert!(config.allowed_syscalls.is_empty());
    }

    #[test]
    fn test_from_manifest_network_enabled() {
        let sandbox = ToolSandbox {
            network: true,
            fs_write: false,
            gpu: false,
            max_memory_mb: 128,
            max_cpu_ms: 10000,
            syscalls: Vec::new(),
            weight: None,
        };
        let config = SandboxConfig::from_manifest(&sandbox);
        assert!(config.allow_network);
        let effective = config.effective_syscalls();
        assert!(effective.contains(&"socket".to_string()));
        assert!(effective.contains(&"connect".to_string()));
        assert!(effective.contains(&"sendto".to_string()));
    }

    #[test]
    fn test_from_manifest_custom_syscalls() {
        let sandbox = ToolSandbox {
            network: false,
            fs_write: false,
            gpu: false,
            max_memory_mb: 64,
            max_cpu_ms: 5000,
            syscalls: vec!["custom_syscall".to_string()],
            weight: None,
        };
        let config = SandboxConfig::from_manifest(&sandbox);
        let effective = config.effective_syscalls();
        assert!(effective.contains(&"custom_syscall".to_string()));
        // Base syscalls should always be present
        assert!(effective.contains(&"read".to_string()));
        assert!(effective.contains(&"write".to_string()));
    }

    #[test]
    fn test_default_config() {
        let config = SandboxConfig::default();
        assert!(!config.allow_network);
        assert!(!config.allow_fs_write);
        assert!(!config.allow_gpu);
        assert_eq!(config.max_memory_bytes, 64 * 1024 * 1024);
    }

    #[test]
    fn test_effective_syscalls_includes_base() {
        let config = SandboxConfig::default();
        let effective = config.effective_syscalls();
        for base in SandboxConfig::default_base_syscalls() {
            assert!(
                effective.contains(&base.to_string()),
                "Missing base syscall: {}",
                base
            );
        }
    }

    #[test]
    fn test_fs_write_adds_syscalls() {
        let config = SandboxConfig {
            allow_fs_write: true,
            ..Default::default()
        };
        let effective = config.effective_syscalls();
        assert!(effective.contains(&"mkdir".to_string()));
        assert!(effective.contains(&"unlink".to_string()));
        assert!(effective.contains(&"rename".to_string()));
    }

    #[test]
    fn test_rlimit_as_bytes_adds_category_overhead() {
        let config = SandboxConfig {
            max_memory_bytes: 4 * 1024 * 1024,
            ..Default::default()
        };

        assert_eq!(
            config.rlimit_as_bytes(SandboxConfig::OVERHEAD_STATELESS),
            SandboxConfig::OVERHEAD_STATELESS + 4 * 1024 * 1024
        );
    }

    #[test]
    fn test_rlimit_as_bytes_uses_baseline_floor() {
        let config = SandboxConfig {
            max_memory_bytes: 0,
            ..Default::default()
        };

        assert_eq!(
            config.rlimit_as_bytes(SandboxConfig::OVERHEAD_NETWORK),
            SandboxConfig::OVERHEAD_NETWORK
        );
    }

    #[test]
    fn test_rlimit_as_bytes_saturates_on_overflow() {
        let config = SandboxConfig {
            max_memory_bytes: u64::MAX,
            ..Default::default()
        };

        assert_eq!(
            config.rlimit_as_bytes(SandboxConfig::OVERHEAD_DEFAULT),
            u64::MAX
        );
    }
}
