use std::path::{Path, PathBuf};

/// Result of a single diagnostic check.
#[derive(Debug)]
pub enum CheckResult {
    Pass(String),
    Warn {
        message: String,
        fix: Option<String>,
    },
    Fail {
        message: String,
        fix: Option<String>,
    },
}

impl CheckResult {
    fn label(&self) -> &str {
        match self {
            CheckResult::Pass(_) => "PASS",
            CheckResult::Warn { .. } => "WARN",
            CheckResult::Fail { .. } => "FAIL",
        }
    }

    fn message(&self) -> &str {
        match self {
            CheckResult::Pass(m) => m,
            CheckResult::Warn { message, .. } | CheckResult::Fail { message, .. } => message,
        }
    }

    fn fix(&self) -> Option<&str> {
        match self {
            CheckResult::Pass(_) => None,
            CheckResult::Warn { fix, .. } | CheckResult::Fail { fix, .. } => fix.as_deref(),
        }
    }

    fn is_fail(&self) -> bool {
        matches!(self, CheckResult::Fail { .. })
    }
}

/// Run all diagnostic checks and print results.
/// Returns `Ok(())` if no check fails; `Err` if any check returns Fail.
pub async fn handle(fix: bool) -> anyhow::Result<()> {
    println!("Running AgentOS diagnostics...\n");

    let checks = run_checks(fix).await;
    let mut has_fail = false;

    for (name, result) in &checks {
        let icon = match result {
            CheckResult::Pass(_) => "✓",
            CheckResult::Warn { .. } => "⚠",
            CheckResult::Fail { .. } => "✗",
        };
        println!("  {} [{:4}] {}", icon, result.label(), name);
        println!("         {}", result.message());
        if let Some(fix_msg) = result.fix() {
            println!("         Fix: {}", fix_msg);
        }
        if result.is_fail() {
            has_fail = true;
        }
    }

    println!();
    if has_fail {
        println!("Some checks failed. Run `agentos doctor --fix` to attempt auto-repair.");
        anyhow::bail!("Diagnostic checks failed");
    } else {
        println!("All checks passed.");
    }
    Ok(())
}

async fn run_checks(fix: bool) -> Vec<(String, CheckResult)> {
    let mut checks = Vec::new();

    let config_path = super::config_cmd::config_path();

    checks.push((
        "Config file exists".to_string(),
        check_config_file(&config_path, fix),
    ));
    checks.push((
        "Config valid TOML".to_string(),
        check_config_parses(&config_path),
    ));

    // Read vault/audit paths from the actual config if parseable.
    let (vault_path, audit_path) = extract_db_paths(&config_path);

    checks.push((
        "Vault database directory".to_string(),
        check_dir_writable(&vault_path, fix),
    ));
    checks.push((
        "Audit log directory".to_string(),
        check_dir_writable(&audit_path, fix),
    ));
    checks.push((
        "IPC socket directory".to_string(),
        check_bus_socket_dir(fix),
    ));
    checks.push(("Core tool manifests".to_string(), check_tools_dir()));

    checks
}

fn check_config_file(path: &Path, fix: bool) -> CheckResult {
    if path.exists() {
        CheckResult::Pass(format!("Found at {}", path.display()))
    } else if fix {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let default_content = "[kernel]\ndefault_task_timeout_secs = 300\n\n\
            [llm]\nprimary = \"\"\nfallbacks = []\n\n\
            [vault]\ndb_path = \"data/vault.db\"\n\n\
            [audit]\ndb_path = \"data/audit.db\"\n";
        match std::fs::write(path, default_content) {
            Ok(_) => CheckResult::Pass(format!("Created default config at {}", path.display())),
            Err(e) => CheckResult::Fail {
                message: format!("Failed to create config: {}", e),
                fix: None,
            },
        }
    } else {
        CheckResult::Fail {
            message: format!("Not found at {}", path.display()),
            fix: Some(
                "Run `agentos doctor --fix` or `agentos onboard` to create config".to_string(),
            ),
        }
    }
}

fn check_config_parses(path: &Path) -> CheckResult {
    if !path.exists() {
        return CheckResult::Warn {
            message: "Config file missing — skipping parse check".to_string(),
            fix: None,
        };
    }
    match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<toml::Value>(&content) {
            Ok(_) => CheckResult::Pass("Parses as valid TOML".to_string()),
            Err(e) => CheckResult::Fail {
                message: format!("TOML parse error: {}", e),
                fix: Some("Edit the config file to fix the syntax error".to_string()),
            },
        },
        Err(e) => CheckResult::Fail {
            message: format!("Cannot read config: {}", e),
            fix: None,
        },
    }
}

/// Extract vault and audit DB paths from the config file.
/// Falls back to defaults if config is missing or unparseable.
fn extract_db_paths(config_path: &Path) -> (PathBuf, PathBuf) {
    let default_vault = PathBuf::from("data/vault.db");
    let default_audit = PathBuf::from("data/audit.db");

    let Ok(content) = std::fs::read_to_string(config_path) else {
        return (default_vault, default_audit);
    };
    let Ok(value) = toml::from_str::<toml::Value>(&content) else {
        return (default_vault, default_audit);
    };

    let vault = value
        .get("vault")
        .and_then(|v| v.get("db_path"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or(default_vault);

    let audit = value
        .get("audit")
        .and_then(|v| v.get("db_path"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or(default_audit);

    (vault, audit)
}

/// Check that the parent directory of a path exists and is writable.
/// Uses an actual write probe rather than just existence check.
fn check_dir_writable(path: &Path, fix: bool) -> CheckResult {
    let parent = path.parent().unwrap_or(Path::new("."));

    if !parent.exists() {
        if fix {
            return match std::fs::create_dir_all(parent) {
                Ok(_) => CheckResult::Pass(format!("Created directory {}", parent.display())),
                Err(e) => CheckResult::Fail {
                    message: format!("Cannot create {}: {}", parent.display(), e),
                    fix: None,
                },
            };
        }
        return CheckResult::Warn {
            message: format!("Directory does not exist: {}", parent.display()),
            fix: Some("Run `agentos doctor --fix` to create it".to_string()),
        };
    }

    // Probe actual writability — existence alone doesn't guarantee it.
    let probe = parent.join(".agentos_write_probe");
    match std::fs::write(&probe, b"") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            CheckResult::Pass(format!("{} exists and is writable", parent.display()))
        }
        Err(e) => CheckResult::Fail {
            message: format!("{} exists but is not writable: {}", parent.display(), e),
            fix: Some("Check directory permissions".to_string()),
        },
    }
}

fn check_bus_socket_dir(fix: bool) -> CheckResult {
    let socket_dir = PathBuf::from("/tmp/agentos");
    if socket_dir.exists() {
        CheckResult::Pass(format!("{} exists", socket_dir.display()))
    } else if fix {
        match std::fs::create_dir_all(&socket_dir) {
            Ok(_) => CheckResult::Pass(format!("Created {}", socket_dir.display())),
            Err(e) => CheckResult::Fail {
                message: format!("Failed to create socket dir: {}", e),
                fix: None,
            },
        }
    } else {
        CheckResult::Warn {
            message: format!("{} not found (kernel not running?)", socket_dir.display()),
            fix: Some("Start the kernel with `agentos init` first".to_string()),
        }
    }
}

fn check_tools_dir() -> CheckResult {
    let tools_dir = PathBuf::from("tools/core");
    if !tools_dir.exists() {
        return CheckResult::Warn {
            message: "tools/core/ directory not found".to_string(),
            fix: Some("Ensure you are running from the AgentOS workspace root".to_string()),
        };
    }
    let count = std::fs::read_dir(&tools_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "toml").unwrap_or(false))
                .count()
        })
        .unwrap_or(0);

    if count == 0 {
        CheckResult::Fail {
            message: "No .toml tool manifests found in tools/core/".to_string(),
            fix: Some("Run the tool installer or check your workspace".to_string()),
        }
    } else {
        CheckResult::Pass(format!("{} core tool manifests found", count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn test_check_config_parses_valid() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "[llm]\nprimary = \"anthropic/claude-opus-4-6\"").unwrap();
        let result = check_config_parses(tmp.path());
        assert!(matches!(result, CheckResult::Pass(_)));
    }

    #[test]
    fn test_check_config_parses_invalid() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "this = [invalid toml").unwrap();
        let result = check_config_parses(tmp.path());
        assert!(matches!(result, CheckResult::Fail { .. }));
    }

    #[test]
    fn test_check_config_parses_missing() {
        let result = check_config_parses(Path::new("/nonexistent/path/config.toml"));
        assert!(matches!(result, CheckResult::Warn { .. }));
    }

    #[test]
    fn test_check_dir_writable_existing() {
        let tmp = TempDir::new().unwrap();
        // Parent directory exists and is writable — probe should succeed.
        let path = tmp.path().join("some_db.db");
        let result = check_dir_writable(&path, false);
        assert!(matches!(result, CheckResult::Pass(_)));
    }

    #[test]
    fn test_check_dir_writable_missing_with_fix() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("new_dir/sub/db.db");
        let result = check_dir_writable(&path, true);
        assert!(matches!(result, CheckResult::Pass(_)));
        assert!(path.parent().unwrap().exists());
    }

    #[test]
    fn test_extract_db_paths_from_config() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "[vault]\ndb_path = \"/custom/vault.db\"\n[audit]\ndb_path = \"/custom/audit.db\""
        )
        .unwrap();
        let (vault, audit) = extract_db_paths(tmp.path());
        assert_eq!(vault, PathBuf::from("/custom/vault.db"));
        assert_eq!(audit, PathBuf::from("/custom/audit.db"));
    }

    #[test]
    fn test_extract_db_paths_defaults_when_missing() {
        let (vault, audit) = extract_db_paths(Path::new("/nonexistent"));
        assert_eq!(vault, PathBuf::from("data/vault.db"));
        assert_eq!(audit, PathBuf::from("data/audit.db"));
    }
}
