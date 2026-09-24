//! Read-only host filesystem view shared by every bwrap sandbox, and the
//! [`Sandbox`] builder every agent-controlled command runs through.
//!
//! `shell-exec`, the kernel's managed env/build/process capabilities, script
//! tools and document extraction all mask `/etc` with an empty tmpfs and then
//! bind back what programs need. When each call site kept its own list they
//! drifted: `shell-exec` bound nothing back, so on
//! Debian/Ubuntu every `/etc/alternatives` symlink dangled — `awk`, `which`,
//! `cc`, `python`, and any binary linked against an alternatives-managed
//! library (`ffmpeg` → `libblas.so.3`) failed to start — and network-enabled
//! commands had no `/etc/resolv.conf`.
//!
//! The lists are an allowlist on purpose. Binding `/etc` wholesale and masking
//! known secrets fails open the day a package drops a new credential file;
//! this fails closed. Anything added must be world-readable, non-secret
//! configuration — `sandbox_lists_expose_no_secrets` enforces the obvious cases.
//!
//! Sandboxes are spawned with `--die-with-parent`, i.e. `PR_SET_PDEATHSIG`,
//! which fires when the *thread* that spawned bwrap exits. Tokio worker threads
//! live as long as the runtime unless `block_in_place` retires one, so that
//! call is banned workspace-wide in `clippy.toml`.

use agentos_types::AgentOSError;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

/// System directories bound read-only. `/lib`, `/lib64`, `/bin` and `/sbin`
/// are symlinks or absent on some hosts, so callers bind only what exists.
pub(crate) const SYSTEM_RO_DIRS: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64"];

/// `/etc` entries a dynamically linked program needs to start and behave
/// normally. Bound in every sandbox. A trailing `*` matches every `/etc` entry
/// with that prefix (bwrap itself has no globbing).
pub(crate) const ETC_RUNTIME: &[&str] = &[
    // Dynamic loader: libraries outside the loader's built-in dirs (e.g.
    // `/usr/lib/x86_64-linux-gnu/pulseaudio`) are only found via the cache.
    "/etc/ld.so.cache",
    // Debian alternatives: commands (`awk`, `which`, `cc`) and libraries
    // (`libblas.so.3`) are symlinks through here. Contains only symlinks, and
    // they resolve inside the sandbox — a target outside the bound dirs
    // simply dangles.
    "/etc/alternatives",
    "/etc/os-release",
    "/etc/localtime",
    "/etc/timezone",
    "/etc/locale.alias",
    // getpwuid()/getgrgid(): git, ssh, python `getpass`, `id`. Password hashes
    // live in `/etc/shadow`, which is never bound.
    "/etc/passwd",
    "/etc/group",
    // Name-service order and local names. Without `nsswitch.conf` glibc tries
    // DNS before `/etc/hosts` and stops on NXDOMAIN; without `hosts`,
    // `localhost` does not resolve inside an unshared network namespace.
    "/etc/nsswitch.conf",
    "/etc/hosts",
    "/etc/host.conf",
    "/etc/gai.conf",
    "/etc/services",
    "/etc/protocols",
    // Public CA bundles and OpenSSL/crypto policy (Debian and Fedora layouts).
    // Offline tools (`openssl verify`, local https) need them too.
    "/etc/ssl/certs",
    "/etc/ssl/openssl.cnf",
    "/etc/ca-certificates",
    "/etc/pki/tls/certs",
    "/etc/pki/tls/cert.pem",
    "/etc/pki/tls/openssl.cnf",
    "/etc/pki/ca-trust",
    "/etc/crypto-policies",
    "/etc/fonts",
    "/etc/mime.types",
    // Package config that `/usr` symlinks into on Debian/Ubuntu: OpenJDK's
    // `lib/jvm.cfg` and `conf/`, ImageMagick's `policy.xml` (its security
    // policy — absent means unrestricted), TeX, XML/SGML catalogs, groff.
    "/etc/java-*",
    "/etc/ImageMagick-*",
    "/etc/texmf",
    "/etc/xml",
    "/etc/sgml",
    "/etc/groff",
];

/// `/etc` entries only meaningful when the sandbox shares the host network.
pub(crate) const ETC_NETWORK: &[&str] = &["/etc/resolv.conf"];

/// `--ro-bind p p` for every path that exists on this host.
///
/// bwrap aborts the whole invocation on a missing bind source, so absent and
/// dangling entries (e.g. no `/lib64`, a broken `resolv.conf` symlink) are
/// skipped. A trailing `*` expands to the matching siblings. Must be emitted
/// after the `--tmpfs` that masks the parent, or the tmpfs shadows the bind.
pub(crate) fn ro_bind_existing(paths: &[&str]) -> Vec<OsString> {
    let mut args = Vec::new();
    let mut bind = |p: &Path| {
        if p.exists() {
            let p = p.as_os_str();
            args.extend([OsString::from("--ro-bind"), p.into(), p.into()]);
        }
    };
    for entry in paths {
        let Some(prefix) = entry.strip_suffix('*') else {
            bind(Path::new(entry));
            continue;
        };
        let prefix = Path::new(prefix);
        let (Some(dir), Some(stem)) = (prefix.parent(), prefix.file_name()) else {
            continue;
        };
        let Ok(read) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut matches: Vec<_> = read
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .as_encoded_bytes()
                    .starts_with(stem.as_encoded_bytes())
            })
            .map(|e| e.path())
            .collect();
        // Deterministic argv — easier to diff in logs and tests.
        matches.sort();
        matches.iter().for_each(|p| bind(p));
    }
    args
}

/// Workspace grants that do not contain `data_dir`. A grant of e.g. the
/// operator's home would otherwise bind the vault, audit log and every agent
/// home into the sandbox; such grants are dropped with a warning.
pub fn grants_outside<'a>(
    grants: &'a [PathBuf],
    data_dir: &'a Path,
) -> impl Iterator<Item = &'a PathBuf> + 'a {
    grants.iter().filter(move |g| {
        let contains = data_dir.starts_with(g)
            || std::fs::canonicalize(data_dir)
                .ok()
                .zip(std::fs::canonicalize(g).ok())
                .is_some_and(|(d, g)| d.starts_with(g));
        if contains {
            tracing::warn!(grant = %g.display(), "sandbox: grant contains the kernel data dir; not binding it");
        }
        !contains
    })
}

/// Bind an agent's live storage zones into a sandbox.
///
/// Zones are the kernel's dynamic directory grants — the conversation shared
/// workspace is one. Every file tool honours them; a sandbox that does not makes
/// "write the script" succeed and "run the script" fail with a path error, which
/// reads as a missing file rather than a missing bind. That is exactly how the
/// 2026-09-21 convo deadlock presented, through three operator approvals.
///
/// Skips: anything already inside `home` (bound by the caller), any zone that
/// *contains* `data_dir` (it would bind the vault, the audit log and every agent
/// home — the [`grants_outside`] rule), and any path that no longer exists,
/// because bwrap aborts the whole call on a missing bind rather than skipping it.
/// Returns the paths actually bound, for the caller to log.
pub fn bind_zones(
    mut sandbox: Sandbox,
    zones: &[(PathBuf, agentos_types::ZoneAccessLevel)],
    home: &Path,
    data_dir: &Path,
) -> (Sandbox, Vec<PathBuf>) {
    let mut bound = Vec::new();
    for (path, access) in zones {
        if path.starts_with(home) {
            continue;
        }
        if grants_outside(std::slice::from_ref(path), data_dir)
            .next()
            .is_none()
        {
            continue; // warned by `grants_outside`
        }
        if !path.exists() {
            tracing::warn!(zone = %path.display(), "sandbox: zone path missing; not binding it");
            continue;
        }
        sandbox = match access {
            agentos_types::ZoneAccessLevel::ReadOnly => sandbox.bind_ro(path),
            agentos_types::ZoneAccessLevel::ReadWrite => sandbox.bind_rw(path),
        };
        bound.push(path.clone());
    }
    (sandbox, bound)
}

/// `PATH` inside every sandbox. The kernel's own `PATH` is never inherited:
/// its entries (pyenv shims, nvm, `~/.local/bin`) live under the masked
/// `/home` and would not resolve anyway.
pub const SANDBOX_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A bwrap invocation: the shared read-only system view, an empty `/home`,
/// `/root`, `/var` and `/tmp`, a cleared environment, and no network unless
/// asked for. Callers add the directories the command may touch.
///
/// ```ignore
/// let mut cmd = Sandbox::new("build-run")
///     .bind_rw(&workspace)
///     .env("HOME", &workspace)
///     .command(&workspace, "cargo")
///     .await?;
/// cmd.args(["build"]);
/// ```
#[derive(Debug, Clone)]
pub struct Sandbox {
    tool: String,
    /// `(path, writable)`; emitted parents-first.
    binds: Vec<(PathBuf, bool)>,
    network: bool,
    path_prepend: Vec<PathBuf>,
    env: Vec<(String, OsString)>,
}

impl Sandbox {
    /// `tool` names the caller in errors and logs.
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            binds: Vec::new(),
            network: false,
            path_prepend: Vec::new(),
            env: Vec::new(),
        }
    }

    /// Expose `path` read-write at the same location. Missing paths are
    /// skipped (bwrap would abort the whole call on them).
    pub fn bind_rw(mut self, path: impl Into<PathBuf>) -> Self {
        self.binds.push((path.into(), true));
        self
    }

    /// Expose `path` read-only at the same location.
    pub fn bind_ro(mut self, path: impl Into<PathBuf>) -> Self {
        self.binds.push((path.into(), false));
        self
    }

    /// Share the host network namespace (and `/etc/resolv.conf`). The caller
    /// is responsible for the permission check.
    pub fn network(mut self, on: bool) -> Self {
        self.network = on;
        self
    }

    /// Put `dir` in front of [`SANDBOX_PATH`]. Later calls go further back.
    pub fn path_prepend(mut self, dir: impl Into<PathBuf>) -> Self {
        self.path_prepend.push(dir.into());
        self
    }

    /// Set an environment variable in the child. `PATH` is managed by
    /// [`Sandbox::path_prepend`] and ignored here.
    pub fn env(mut self, key: impl Into<String>, value: impl AsRef<OsStr>) -> Self {
        self.env.push((key.into(), value.as_ref().to_owned()));
        self
    }

    /// Build the command. Arguments for `program` go on the returned command.
    /// Fails closed when bwrap cannot sandbox on this host.
    pub async fn command(
        self,
        chdir: &Path,
        program: impl AsRef<OsStr>,
    ) -> Result<tokio::process::Command, AgentOSError> {
        if !bwrap_usable().await {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: self.tool,
                reason: "bwrap (bubblewrap) is not installed or cannot create namespaces on this \
                         host. Agent commands require sandbox isolation and never run without it. \
                         Install bubblewrap and allow unprivileged user namespaces."
                    .into(),
            });
        }
        let mut cmd = tokio::process::Command::new("bwrap");
        // `--clearenv` only scrubs the child; bwrap itself would otherwise keep
        // the kernel's API keys in `/proc/<pid>/environ` for its lifetime.
        cmd.env_clear().env("PATH", SANDBOX_PATH);
        cmd.args(self.args(chdir, host_rustup()));
        cmd.arg(program);
        // No orphan when the future is dropped (timeout, cancellation).
        cmd.kill_on_drop(true);
        Ok(cmd)
    }

    /// Everything up to and including `--`. Split out so tests can inspect it.
    fn args(self, chdir: &Path, rustup: Option<&Rustup>) -> Vec<OsString> {
        let mut a: Vec<OsString> = ro_bind_existing(SYSTEM_RO_DIRS);
        // Mask before binding back: bwrap applies mounts in argv order, so a
        // bind under one of these must come after its tmpfs.
        for dir in ["/root", "/etc", "/var", "/home", "/tmp"] {
            a.extend(["--tmpfs".into(), dir.into()]);
        }
        a.extend(ro_bind_existing(ETC_RUNTIME));
        if self.network {
            a.extend(ro_bind_existing(ETC_NETWORK));
        }

        let mut binds = self.binds;
        let mut path = self.path_prepend;
        let mut env = self.env;
        if let Some(r) = rustup {
            binds.push((r.home.clone(), false));
            binds.push((r.cargo_bin.clone(), false));
            path.push(r.cargo_bin.clone());
            env.push(("RUSTUP_HOME".into(), r.home.clone().into()));
        }
        // SECURITY: bwrap resolves a bind *source* on the host (following
        // symlinks) and its *destination* inside the half-built root. A bind
        // nested under a writable bind therefore has a source the sandboxed
        // agent can swap for a symlink (`ws -> ../../..`) and a destination it
        // can redirect, so the next sandbox would mount any host directory
        // read-write. Nested binds are dropped — the writable ancestor already
        // exposes them — and symlinked sources are refused. Parents-first
        // order makes the ancestor check a single pass.
        binds.sort_by_key(|(p, _)| p.components().count());
        let mut writable: Vec<&Path> = Vec::new();
        for (p, rw) in &binds {
            if !p.is_absolute() || p.components().any(|c| c == Component::ParentDir) {
                tracing::warn!(tool = %self.tool, path = %p.display(), "sandbox: refusing non-absolute or `..` bind");
                continue;
            }
            if let Some(parent) = writable.iter().find(|w| p.starts_with(w)) {
                tracing::debug!(tool = %self.tool, path = %p.display(), parent = %parent.display(), "sandbox: bind covered by a writable ancestor; skipped");
                continue;
            }
            match std::fs::symlink_metadata(p) {
                Ok(m) if m.file_type().is_symlink() => {
                    tracing::warn!(tool = %self.tool, path = %p.display(), "sandbox: refusing symlinked bind source");
                    continue;
                }
                Ok(_) => {}
                Err(_) => {
                    tracing::debug!(tool = %self.tool, path = %p.display(), "sandbox: bind source missing; skipped");
                    continue;
                }
            }
            let flag = if *rw { "--bind" } else { "--ro-bind" };
            a.extend([flag.into(), p.into(), p.into()]);
            if *rw {
                writable.push(p);
            }
        }

        a.extend(["--dev", "/dev", "--proc", "/proc", "--unshare-all"].map(OsString::from));
        // No controlling terminal (TIOCSTI injection), and no orphaned sandbox
        // when the kernel is SIGKILLed.
        a.extend(["--new-session", "--die-with-parent"].map(OsString::from));
        if self.network {
            a.push("--share-net".into());
        }

        // The kernel environment holds every provider API key; none of it
        // reaches the child.
        let mut path_var = OsString::new();
        for dir in &path {
            path_var.push(dir);
            path_var.push(":");
        }
        path_var.push(SANDBOX_PATH);
        a.push("--clearenv".into());
        let base = [
            ("TMPDIR", OsString::from("/tmp")),
            ("LANG", "C.UTF-8".into()),
        ];
        for (k, v) in base.into_iter().map(|(k, v)| (k.to_string(), v)).chain(env) {
            if k != "PATH" {
                a.extend(["--setenv".into(), k.into(), v]);
            }
        }
        a.extend(["--setenv".into(), "PATH".into(), path_var]);
        a.extend(["--chdir".into(), chdir.into(), "--".into()]);
        a
    }
}

/// A host rustup install, exposed read-only so `cargo`/`rustc` work in the
/// sandbox. Only `$CARGO_HOME/bin` is bound — `$CARGO_HOME` itself holds
/// `credentials.toml`. `CARGO_HOME` inside defaults to `$HOME/.cargo`, i.e.
/// the caller's writable home.
#[derive(Debug)]
struct Rustup {
    home: PathBuf,
    cargo_bin: PathBuf,
}

// ponytail: rustup is the one toolchain manager handled; nvm/pyenv installs
// under /home stay invisible (system node/python are used). Add a resolver
// here if a host needs one.
fn host_rustup() -> Option<&'static Rustup> {
    static RUSTUP: std::sync::OnceLock<Option<Rustup>> = std::sync::OnceLock::new();
    RUSTUP
        .get_or_init(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let rustup_home = std::env::var_os("RUSTUP_HOME")
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(".rustup")))?;
            let cargo_bin = std::env::var_os("CARGO_HOME")
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(".cargo")))?
                .join("bin");
            // A misconfigured RUSTUP_HOME (e.g. `$HOME`) must not expose a
            // whole home directory: require the rustup layout.
            (rustup_home.is_absolute()
                && cargo_bin.is_absolute()
                && rustup_home.join("toolchains").is_dir()
                && cargo_bin.is_dir())
            .then_some(Rustup {
                home: rustup_home,
                cargo_bin,
            })
        })
        .as_ref()
}

/// Whether bwrap can actually create a sandbox on this host. Probed once.
///
/// `bwrap --version` is not enough: Docker's default seccomp profile and
/// distros with unprivileged user namespaces disabled ship a working
/// `--version` and then fail every real invocation. A `false` here must mean
/// "cannot sandbox", never "probe misconfigured" — extract falls back to
/// running converters unsandboxed on it.
pub async fn bwrap_usable() -> bool {
    static USABLE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
    *USABLE
        .get_or_init(|| async {
            // Bind the same system dirs the real sandboxes do. A bare
            // `--ro-bind /usr /usr` cannot exec anything on a merged-/usr host:
            // `/bin` and the dynamic loader in `/lib64` are symlinks outside
            // it, so the probe failed everywhere and document converters ran
            // unsandboxed.
            let ok = tokio::process::Command::new("bwrap")
                .args(ro_bind_existing(SYSTEM_RO_DIRS))
                // Same flags as real invocations: `--proc` fails in many
                // containers and `--clearenv` needs bwrap >= 0.5, and a probe
                // that passes where calls fail turns "no sandbox" into
                // cryptic per-call errors.
                .args(["--dev", "/dev", "--proc", "/proc", "--unshare-all"])
                .args(["--new-session", "--die-with-parent", "--clearenv"])
                .args(["--", "/bin/sh", "-c", "exit 0"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                tracing::warn!(
                    "bwrap is missing or cannot create a namespace here — shell-exec and script \
                     tools are disabled, and document converters run unsandboxed. Install \
                     bubblewrap, and on a container host allow unprivileged user namespaces."
                );
            }
            ok
        })
        .await
}

/// Whether this bwrap accepts `--size` for a tmpfs (bubblewrap >= 0.9).
/// Older versions reject the entire invocation on the unknown option, so
/// callers must not pass it blind. Probed once; `false` when bwrap is unusable.
pub(crate) async fn bwrap_supports_tmpfs_size() -> bool {
    static SIZED: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
    *SIZED
        .get_or_init(|| async {
            bwrap_usable().await
                && tokio::process::Command::new("bwrap")
                    .args(ro_bind_existing(SYSTEM_RO_DIRS))
                    .args(["--size", "1048576", "--tmpfs", "/tmp", "--unshare-all"])
                    .args(["--", "/bin/sh", "-c", "exit 0"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await
                    .is_ok_and(|s| s.success())
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_lists_expose_no_secrets() {
        const SECRET: &[&str] = &[
            "/etc/shadow",
            "/etc/gshadow",
            "/etc/sudoers",
            "/etc/ssh",
            "/etc/ssl/private",
            "/etc/pki/tls/private",
            "/etc/environment",
            "/etc/NetworkManager",
            "/etc/wpa_supplicant",
            "/etc/security",
            "/etc/machine-id",
        ];
        for entry in SYSTEM_RO_DIRS.iter().chain(ETC_RUNTIME).chain(ETC_NETWORK) {
            let prefix = entry.trim_end_matches('*');
            // Rejects the secret itself, anything inside it, any parent
            // directory of it (`/etc`, `/etc/ssl`, `/etc/pki`), and a prefix
            // pattern that could match it (`/etc/s*`).
            for secret in SECRET {
                assert!(
                    !prefix.is_empty()
                        && !secret.starts_with(prefix)
                        && !prefix.starts_with(&format!("{secret}/")),
                    "{entry} exposes {secret}"
                );
            }
        }
    }

    fn strs(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn pos(args: &[String], needle: &[&str]) -> usize {
        args.windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| panic!("{needle:?} not in {args:?}"))
    }

    #[test]
    fn sandbox_args_order_binds_scrub_env_and_gate_network() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let grant = home.join("ro-grant");
        std::fs::create_dir_all(&grant).unwrap();
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let rustup = Rustup {
            home: dir.path().join(".rustup"),
            cargo_bin: dir.path().join("cargo-bin"),
        };
        std::fs::create_dir_all(&rustup.home).unwrap();
        std::fs::create_dir_all(&rustup.cargo_bin).unwrap();

        let ro_top = dir.path().join("ro-top");
        let ro_child = ro_top.join("rw-child");
        std::fs::create_dir_all(&ro_child).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&home, &link).unwrap();

        let args = strs(
            &Sandbox::new("t")
                // Registered before its writable parent: must still be dropped.
                .bind_ro(&grant)
                .bind_rw(home.join("ws"))
                .bind_rw(&ro_child)
                .bind_ro(&ro_top)
                .bind_rw(&link)
                .bind_rw(&home)
                .bind_rw(dir.path().join("missing"))
                .bind_rw("relative/path")
                .bind_rw(home.join("..").join("escape"))
                .path_prepend(home.join("bin"))
                .env("HOME", &home)
                .env("PATH", "/evil")
                .args(&home, Some(&rustup)),
        );

        let home_bind = pos(&args, &["--bind", &s(&home), &s(&home)]);
        assert!(pos(&args, &["--tmpfs", "/home"]) < home_bind);
        // Nested under a writable bind: swappable for a symlink from inside,
        // so never bound on its own.
        assert!(!args.iter().any(|a| a.contains("ro-grant")));
        // Under a read-only bind the agent cannot touch: kept, after it.
        let ro_top_bind = pos(&args, &["--ro-bind", &s(&ro_top), &s(&ro_top)]);
        assert!(ro_top_bind < pos(&args, &["--bind", &s(&ro_child), &s(&ro_child)]));
        // A symlinked source would be resolved on the host.
        assert!(!args.iter().any(|a| a == &s(&link)));
        pos(&args, &["--ro-bind", &s(&rustup.home), &s(&rustup.home)]);
        for bad in ["missing", "relative/path", "escape", "/ws"] {
            assert!(!args.iter().any(|a| a.contains(bad)), "{bad} bound");
        }
        assert!(!args
            .iter()
            .any(|a| a == "--share-net" || a == "/etc/resolv.conf"));

        // Environment: cleared, then only what was set; PATH is ours.
        let clear = pos(&args, &["--clearenv"]);
        assert!(clear < pos(&args, &["--setenv", "HOME", &s(&home)]));
        let path = format!(
            "{}:{}:{SANDBOX_PATH}",
            s(&home.join("bin")),
            s(&rustup.cargo_bin)
        );
        pos(&args, &["--setenv", "PATH", &path]);
        pos(&args, &["--setenv", "RUSTUP_HOME", &s(&rustup.home)]);
        assert!(!args.iter().any(|a| a == "/evil"));
        assert_eq!(&args[args.len() - 3..], ["--chdir", &s(&home), "--"]);

        let net = strs(&Sandbox::new("t").network(true).args(&home, None));
        pos(&net, &["--share-net"]);
        if Path::new("/etc/resolv.conf").exists() {
            pos(&net, &["--ro-bind", "/etc/resolv.conf", "/etc/resolv.conf"]);
        }
    }

    #[tokio::test]
    async fn sandbox_command_hides_kernel_env_and_unbound_paths() {
        if !bwrap_usable().await {
            eprintln!("skipping: bwrap cannot sandbox on this host");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (home, hidden) = (dir.path().join("home"), dir.path().join("hidden"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(dir.path().join("hidden"), "secret").unwrap();
        // `cargo test` exports this to the test process (standing in for the
        // kernel's API keys); it must not reach the child.
        let parent_var = "CARGO_MANIFEST_DIR";
        assert!(std::env::var_os(parent_var).is_some());

        let mut cmd = Sandbox::new("t")
            .bind_rw(&home)
            .env("HOME", &home)
            .command(&home, "/bin/sh")
            .await
            .unwrap();
        cmd.args([
            "-c",
            &format!(
                "env; test -e '{}' && echo HIDDEN_VISIBLE; echo ok > out",
                hidden.display()
            ),
        ]);
        let out = cmd.output().await.unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains(parent_var), "{stdout}");
        assert!(!stdout.contains("HIDDEN_VISIBLE"));
        assert!(stdout.contains(&format!("HOME={}", home.display())));
        assert_eq!(std::fs::read_to_string(home.join("out")).unwrap(), "ok\n");
    }

    #[test]
    fn ro_bind_existing_skips_missing_sources_and_expands_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("present");
        std::fs::write(&present, b"").unwrap();
        for d in ["java-17", "java-21", "javascript"] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();

        let s = |p: &Path| p.to_string_lossy().into_owned();
        let pattern = format!("{}/java-*", s(dir.path()));
        let args = ro_bind_existing(&[
            &s(&present),
            "/definitely/not/here",
            &s(&dangling),
            &pattern,
        ]);

        let expected: Vec<OsString> = [
            present,
            dir.path().join("java-17"),
            dir.path().join("java-21"),
        ]
        .iter()
        .flat_map(|p| ["--ro-bind".to_string(), s(p), s(p)])
        .map(OsString::from)
        .collect();
        assert_eq!(args, expected);
    }
}
