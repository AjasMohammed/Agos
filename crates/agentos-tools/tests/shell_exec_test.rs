use agentos_tools::shell_exec::ShellExec;
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
use std::path::Path;
use tempfile::TempDir;

fn make_context(data_dir: &Path) -> ToolExecutionContext {
    ToolExecutionContext {
        data_dir: data_dir.to_path_buf(),
        task_id: TaskID::new(),
        agent_id: AgentID::new(),
        trace_id: TraceID::new(),
        permissions: PermissionSet::new(),
        vault: None,
        hal: None,
        file_lock_registry: None,
        agent_registry: None,
        task_registry: None,
        escalation_query: None,
        workspace_paths: vec![],
        workspace_paths_writable: vec![],
        workspace_paths_executable: vec![],
        capability_registry: None,
        capability_dispatcher: None,
        storage_zone_query: None,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tool_categories: None,
        shared_dir: None,
    }
}

#[tokio::test]
async fn test_shell_exec_bwrap_root() {
    let dir = TempDir::new().unwrap();
    let tool = ShellExec::new();

    // Check if bwrap exists, otherwise this test is meaningless
    if !agentos_tools::sandbox_fs::bwrap_usable().await {
        println!("Skipping bwrap test because bwrap is not installed");
        return;
    }

    let result = tool
        .execute(
            serde_json::json!({"command": "ls /root"}),
            make_context(dir.path()),
        )
        .await
        .unwrap();

    let stderr = result["stderr"].as_str().unwrap();
    assert!(
        stderr.contains("No such file or directory")
            || stderr.contains("Permission denied")
            || result["stdout"].as_str().unwrap().is_empty(),
        "Should not be able to list /root. Got stderr: {}, stdout: {}",
        stderr,
        result["stdout"]
    );
}

#[tokio::test]
async fn test_shell_exec_bwrap_etc() {
    let dir = TempDir::new().unwrap();
    let tool = ShellExec::new();

    if !agentos_tools::sandbox_fs::bwrap_usable().await {
        return;
    }

    let result = tool
        .execute(
            serde_json::json!({"command": "cat /etc/shadow"}),
            make_context(dir.path()),
        )
        .await
        .unwrap();

    let stderr = result["stderr"].as_str().unwrap();
    assert!(
        stderr.contains("No such file or directory") || stderr.contains("Permission denied"),
        "Should not be able to read /etc/shadow. Got stderr: {}",
        stderr
    );
}

/// `bwrap --version` succeeds on hosts where namespaces are blocked (Docker
/// seccomp, userns disabled); only a real sandbox proves the tests can run.
fn bwrap_usable() -> bool {
    std::process::Command::new("bwrap")
        .args(["--ro-bind", "/usr", "/usr"])
        .args(
            ["/bin", "/sbin", "/lib", "/lib64"]
                .iter()
                .filter(|d| Path::new(d).exists())
                .flat_map(|d| ["--ro-bind", d, d]),
        )
        .args(["--unshare-all", "--", "/bin/sh", "-c", "exit 0"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn run(command: &str, ctx: ToolExecutionContext, allow_network: bool) -> serde_json::Value {
    let result = ShellExec::new()
        .execute(
            serde_json::json!({"command": command, "allow_network": allow_network}),
            ctx,
        )
        .await
        .unwrap();
    // Every probe below ends in `true`; a failure means bwrap itself refused
    // the invocation and an empty stdout would prove nothing.
    assert_eq!(
        result["success"], true,
        "sandbox failed: {}",
        result["stderr"]
    );
    result
}

/// Symlinks under `dir` (to `depth` levels) whose target is in `/etc` and whose
/// final destination is inside what the sandbox binds — the host resolves
/// them, so the sandbox must too.
fn etc_links(dir: &Path, depth: u32, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            // `join` keeps an absolute target as-is and anchors a relative one
            // (`../../etc/java-17/jvm.cfg`); canonicalizing its directory
            // normalises the `..` without following the link itself.
            let into_etc = std::fs::read_link(&path)
                .ok()
                .and_then(|t| path.parent()?.join(t).parent().map(Path::to_path_buf))
                .and_then(|d| std::fs::canonicalize(d).ok())
                .is_some_and(|d| d.starts_with("/etc"));
            // `/opt` browsers behind x-www-browser are deliberately unbound.
            let lands_inside = std::fs::canonicalize(&path).is_ok_and(|t| {
                ["/usr", "/bin", "/sbin", "/lib", "/etc"]
                    .iter()
                    .any(|d| t.starts_with(d))
            });
            if into_etc && lands_inside {
                out.push(path.to_string_lossy().into_owned());
            }
        } else if ft.is_dir() && depth > 0 {
            etc_links(&path, depth - 1, out);
        }
    }
}

/// Debian/Ubuntu route commands (`awk`, `which`, `cc`) and shared libraries
/// (`libblas.so.3`, which `ffmpeg` links) through `/etc/alternatives`, and
/// packages like OpenJDK symlink `/usr/lib/jvm/*/conf` into `/etc/java-*`.
/// With an empty `/etc` every one of those dangles and the program reports
/// "not found" or exits 127 at load time.
#[tokio::test]
async fn test_shell_exec_resolves_usr_symlinks_into_etc() {
    if !bwrap_usable() {
        return;
    }
    let mut links = Vec::new();
    etc_links(Path::new("/usr/bin"), 0, &mut links);
    etc_links(Path::new("/usr/lib/jvm"), 2, &mut links);
    if links.is_empty() {
        println!("Skipping: host has no /usr -> /etc symlinks");
        return;
    }

    let dir = TempDir::new().unwrap();
    let script = links
        .iter()
        .map(|p| format!("[ -e '{p}' ] || echo 'DANGLING {p}';"))
        .collect::<String>()
        + " true";
    let result = run(&script, make_context(dir.path()), false).await;

    assert_eq!(result["stdout"], "", "stderr: {}", result["stderr"]);
}

#[tokio::test]
async fn test_shell_exec_etc_exposes_no_secrets() {
    if !bwrap_usable() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let result = run(
        "for p in /etc/shadow /etc/gshadow /etc/sudoers /etc/ssh /etc/environment \
         /etc/ssl/private /etc/NetworkManager /etc/machine-id; do \
         [ -e \"$p\" ] && echo \"VISIBLE $p\"; done; true",
        make_context(dir.path()),
        false,
    )
    .await;
    assert_eq!(result["stdout"], "", "stderr: {}", result["stderr"]);
}

#[tokio::test]
async fn test_shell_exec_localhost_resolves_without_network() {
    if !bwrap_usable() || !Path::new("/etc/hosts").exists() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let result = run(
        "getent hosts localhost >/dev/null && echo RESOLVED; true",
        make_context(dir.path()),
        false,
    )
    .await;
    assert_eq!(
        result["stdout"], "RESOLVED\n",
        "stderr: {}",
        result["stderr"]
    );
}

#[tokio::test]
async fn test_shell_exec_resolv_conf_only_with_network() {
    if !bwrap_usable() || !Path::new("/etc/resolv.conf").exists() {
        return;
    }
    let probe = "[ -e /etc/resolv.conf ] && echo PRESENT; true";

    let dir = TempDir::new().unwrap();
    let isolated = run(probe, make_context(dir.path()), false).await;
    assert_eq!(isolated["stdout"], "", "stderr: {}", isolated["stderr"]);

    let mut ctx = make_context(dir.path());
    ctx.permissions
        .grant("network.outbound".to_string(), false, false, true, None);
    let networked = run(probe, ctx, true).await;
    assert_eq!(
        networked["stdout"], "PRESENT\n",
        "stderr: {}",
        networked["stderr"]
    );
}
