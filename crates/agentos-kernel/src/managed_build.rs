//! Managed Builds capability provider (`build.*`).
//!
//! Enables agents to compile code, run tests, execute linters, and retrieve
//! artifacts. Build output is parsed into structured JSON (test results,
//! compiler errors) rather than returned as raw stdout.

use crate::capability_provider::{CapabilityContext, CapabilityProvider, CapabilityResult};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Status of a build execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    Success,
    Failed,
    Timeout,
}

/// Structured test summary parsed from build output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub ignored: u32,
    pub failures: Vec<TestFailure>,
}

/// A single test failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    pub name: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

/// A compiler or linter diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// Severity level of a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Error,
    Warning,
    Info,
    Hint,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the managed build capability.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BuildConfig {
    /// Allowed build command prefixes. Empty = allow all.
    #[serde(default = "default_allowed_commands")]
    pub allowed_commands: Vec<String>,
    /// Build timeout in seconds.
    #[serde(default = "default_build_timeout")]
    pub build_timeout_secs: u64,
    /// Maximum build output capture size in bytes.
    #[serde(default = "default_output_max")]
    pub build_output_max_bytes: usize,
}

fn default_allowed_commands() -> Vec<String> {
    vec![
        "cargo build",
        "cargo test",
        "cargo clippy",
        "cargo fmt",
        "cargo check",
        "cargo run",
        "python -m pytest",
        "pytest",
        "python -m unittest",
        "python -m flake8",
        "pip install",
        "npm run",
        "npm test",
        "npm install",
        "npx jest",
        "npx eslint",
        "npx prettier",
        "make",
        "cmake",
        "go build",
        "go test",
        "go vet",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn default_build_timeout() -> u64 {
    300 // 5 minutes
}
fn default_output_max() -> usize {
    10_485_760 // 10 MB
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            allowed_commands: default_allowed_commands(),
            build_timeout_secs: default_build_timeout(),
            build_output_max_bytes: default_output_max(),
        }
    }
}

// ---------------------------------------------------------------------------
// Output parsers
// ---------------------------------------------------------------------------

/// Detect ecosystem from a working directory.
fn detect_ecosystem(dir: &Path) -> &'static str {
    if dir.join("Cargo.toml").exists() {
        "rust"
    } else if dir.join("package.json").exists() {
        "nodejs"
    } else if dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
        || dir.join("pytest.ini").exists()
        || dir.join("setup.cfg").exists()
    {
        "python"
    } else if dir.join("go.mod").exists() {
        "go"
    } else if dir.join("Makefile").exists() {
        "make"
    } else {
        "unknown"
    }
}

/// Parse cargo test output for test summary.
fn parse_cargo_test_output(output: &str) -> Option<TestSummary> {
    // Look for the summary line: "test result: ok. X passed; Y failed; Z ignored"
    for line in output.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with("test result:") {
            let mut passed = 0u32;
            let mut failed = 0u32;
            let mut ignored = 0u32;

            // Extract numbers from patterns like "5 passed", "1 failed", "0 ignored"
            for part in trimmed.split(';') {
                let part = part.trim();
                // Find the last word and its preceding number
                let words: Vec<&str> = part.split_whitespace().collect();
                for window in words.windows(2) {
                    if let Ok(n) = window[0].parse::<u32>() {
                        match window[1] {
                            "passed" => passed = n,
                            "failed" => failed = n,
                            "ignored" => ignored = n,
                            _ => {}
                        }
                    }
                }
            }

            let mut failures = Vec::new();
            // Parse individual failure lines: "--- module::test_name FAILED"
            for fail_line in output.lines() {
                if let Some(name) = fail_line
                    .trim()
                    .strip_prefix("--- ")
                    .and_then(|s| s.strip_suffix(" FAILED"))
                {
                    failures.push(TestFailure {
                        name: name.to_string(),
                        message: String::new(),
                        file: None,
                        line: None,
                    });
                }
            }

            return Some(TestSummary {
                total: passed + failed + ignored,
                passed,
                failed,
                ignored,
                failures,
            });
        }
    }
    None
}

/// Parse pytest output for test summary.
fn parse_pytest_output(output: &str) -> Option<TestSummary> {
    // Look for summary line: "X passed, Y failed, Z warnings in N.NNs"
    // or "X passed in N.NNs"
    for line in output.lines().rev() {
        let trimmed = line.trim();
        if (trimmed.contains("passed") || trimmed.contains("failed")) && trimmed.contains(" in ") {
            let mut passed = 0u32;
            let mut failed = 0u32;

            for part in trimmed.split(',') {
                let part = part.trim();
                let words: Vec<&str> = part.split_whitespace().collect();
                for window in words.windows(2) {
                    if let Ok(n) = window[0].parse::<u32>() {
                        match window[1].trim_end_matches(',') {
                            "passed" => passed = n,
                            "failed" => failed = n,
                            _ => {}
                        }
                    }
                }
            }

            // Parse FAILED lines
            let mut failures = Vec::new();
            for fail_line in output.lines() {
                let fl = fail_line.trim();
                if fl.starts_with("FAILED ") {
                    let name = fl.strip_prefix("FAILED ").unwrap_or(fl);
                    // Split on " - " to get name and message
                    let (test_name, msg) = name
                        .split_once(" - ")
                        .map(|(n, m)| (n.to_string(), m.to_string()))
                        .unwrap_or_else(|| (name.to_string(), String::new()));
                    failures.push(TestFailure {
                        name: test_name,
                        message: msg,
                        file: None,
                        line: None,
                    });
                }
            }

            return Some(TestSummary {
                total: passed + failed,
                passed,
                failed,
                ignored: 0,
                failures,
            });
        }
    }
    None
}

/// Try parsing output as test results from any supported ecosystem.
fn parse_test_output(output: &str, ecosystem: &str) -> Option<TestSummary> {
    match ecosystem {
        "rust" => parse_cargo_test_output(output),
        "python" => parse_pytest_output(output),
        _ => {
            // Try all parsers
            parse_cargo_test_output(output).or_else(|| parse_pytest_output(output))
        }
    }
}

// ---------------------------------------------------------------------------
// Command validation
// ---------------------------------------------------------------------------

fn validate_build_command(command: &str, allowed: &[String]) -> Result<(), AgentOSError> {
    if allowed.is_empty() {
        return Ok(()); // Empty = allow all
    }

    // Check that the command matches an allowed prefix at a word boundary
    // (exact match or followed by a space). This prevents "make" from matching
    // "make-malicious" and ensures "cargo test" doesn't match "cargo testing".
    if allowed
        .iter()
        .any(|prefix| command == prefix || command.starts_with(&format!("{prefix} ")))
    {
        return Ok(());
    }

    Err(AgentOSError::PermissionDenied {
        resource: "build.run".into(),
        operation: format!(
            "command '{}' is not on the allowed build commands list",
            command.chars().take(100).collect::<String>()
        ),
    })
}

// ---------------------------------------------------------------------------
// BuildProvider
// ---------------------------------------------------------------------------

/// Managed builds capability provider.
pub struct BuildProvider {
    config: BuildConfig,
}

impl BuildProvider {
    pub fn new(config: BuildConfig) -> Self {
        Self { config }
    }

    /// Validate that `working_dir` is within the agent's accessible scope.
    fn validate_working_dir(
        working_dir: &Path,
        context: &CapabilityContext,
    ) -> Result<(), AgentOSError> {
        if !working_dir.starts_with(&context.data_dir)
            && !context
                .workspace_paths
                .iter()
                .any(|wp| working_dir.starts_with(wp))
        {
            let mut allowed: Vec<String> = vec![context.data_dir.display().to_string()];
            for wp in &context.workspace_paths {
                allowed.push(wp.display().to_string());
            }
            return Err(AgentOSError::PermissionDenied {
                resource: "build.run".into(),
                operation: format!(
                    "working_dir '{}' is outside agent scope. Allowed paths: [{}]",
                    working_dir.display(),
                    allowed.join(", ")
                ),
            });
        }
        Ok(())
    }

    pub fn with_defaults() -> Self {
        Self::new(BuildConfig::default())
    }

    async fn action_run(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let command = params["command"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'command' field".into()))?;

        let working_dir = params["working_dir"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| context.data_dir.clone());

        // SECURITY: validate working_dir is within agent's scope.
        Self::validate_working_dir(&working_dir, context)?;

        // Validate command against allowlist.
        validate_build_command(command, &self.config.allowed_commands)?;

        let start = Instant::now();
        let timeout = std::time::Duration::from_secs(self.config.build_timeout_secs);

        // Split command into program + args for safe execution (no sh -c).
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Err(AgentOSError::SchemaValidation("empty build command".into()));
        }

        let program = parts[0];
        let args = &parts[1..];

        let output = tokio::time::timeout(timeout, async {
            tokio::process::Command::new(program)
                .args(args)
                .current_dir(&working_dir)
                .output()
                .await
        })
        .await
        .map_err(|_| AgentOSError::ToolExecutionFailed {
            tool_name: "build-run".into(),
            reason: format!("build timed out after {}s", self.config.build_timeout_secs),
        })?
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "build-run".into(),
            reason: format!("failed to execute build command: {e}"),
        })?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let exit_code = output.status.code().unwrap_or(-1);

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}\n{stderr}");

        // Truncate output
        let max = self.config.build_output_max_bytes;
        let truncated_output = if combined.len() > max {
            combined.chars().take(max).collect::<String>()
        } else {
            combined.clone()
        };

        let status = if output.status.success() {
            BuildStatus::Success
        } else {
            BuildStatus::Failed
        };

        // Try to parse test results
        let ecosystem = detect_ecosystem(&working_dir);
        let test_summary = parse_test_output(&combined, ecosystem);

        Ok(CapabilityResult {
            output: json!({
                "status": serde_json::to_value(status).unwrap_or(Value::Null),
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "tests": test_summary.as_ref().and_then(|t| serde_json::to_value(t).ok()),
                "output": truncated_output,
                "truncated": combined.len() > max,
            }),
            audit_metadata: json!({
                "event": if output.status.success() { "BuildExecuted" } else { "BuildFailed" },
                "command": command,
                "working_dir": working_dir.to_string_lossy().to_string(),
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "agent_id": context.agent_id.to_string(),
            }),
        })
    }

    async fn action_test(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let working_dir = params["working_dir"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| context.data_dir.clone());

        // SECURITY: validate working_dir BEFORE detect_ecosystem to prevent
        // filesystem existence oracle (probing /root/Cargo.toml etc.).
        Self::validate_working_dir(&working_dir, context)?;

        // Auto-detect ecosystem and choose test command.
        let ecosystem = detect_ecosystem(&working_dir);
        let command = match ecosystem {
            "rust" => "cargo test",
            "python" => "pytest",
            "nodejs" => "npm test",
            "go" => "go test ./...",
            "make" => "make test",
            _ => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "cannot auto-detect test command for ecosystem '{ecosystem}'; \
                     use build.run with an explicit command instead"
                )));
            }
        };

        // Delegate to action_run with the detected command.
        let run_params = json!({
            "command": command,
            "working_dir": working_dir.to_string_lossy().to_string(),
        });
        self.action_run(&run_params, context).await
    }

    async fn action_lint(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let working_dir = params["working_dir"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| context.data_dir.clone());

        // SECURITY: validate working_dir BEFORE detect_ecosystem.
        Self::validate_working_dir(&working_dir, context)?;

        let ecosystem = detect_ecosystem(&working_dir);
        let command = match ecosystem {
            "rust" => "cargo clippy",
            "python" => "python -m flake8 .",
            "nodejs" => "npx eslint .",
            "go" => "go vet ./...",
            _ => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "cannot auto-detect lint command for ecosystem '{ecosystem}'; \
                     use build.run with an explicit command instead"
                )));
            }
        };

        let run_params = json!({
            "command": command,
            "working_dir": working_dir.to_string_lossy().to_string(),
        });
        self.action_run(&run_params, context).await
    }
}

#[async_trait]
impl CapabilityProvider for BuildProvider {
    fn domain(&self) -> &str {
        "build"
    }

    fn supported_actions(&self) -> &[&str] {
        &["run", "test", "lint"]
    }

    fn required_permissions(&self, action: &str) -> Option<Vec<(String, PermissionOp)>> {
        match action {
            "run" => Some(vec![("build.run".to_string(), PermissionOp::Execute)]),
            "test" => Some(vec![("build.test".to_string(), PermissionOp::Execute)]),
            "lint" => Some(vec![("build.lint".to_string(), PermissionOp::Execute)]),
            _ => None,
        }
    }

    async fn execute(
        &self,
        action: &str,
        params: Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        match action {
            "run" => self.action_run(&params, context).await,
            "test" => self.action_test(&params, context).await,
            "lint" => self.action_lint(&params, context).await,
            other => Err(AgentOSError::KernelError {
                reason: format!("unknown build action '{other}'"),
            }),
        }
    }

    fn description(&self) -> &str {
        "Compile code, run tests, and execute linters with structured output parsing"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, TaskID, TraceID};

    fn make_provider() -> BuildProvider {
        BuildProvider::with_defaults()
    }

    fn make_context() -> CapabilityContext {
        CapabilityContext {
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
            data_dir: PathBuf::from("/tmp"),
            permissions: agentos_types::PermissionSet::default(),
            workspace_paths: vec![],
        }
    }

    #[test]
    fn provider_metadata() {
        let p = make_provider();
        assert_eq!(p.domain(), "build");
        assert_eq!(p.supported_actions(), &["run", "test", "lint"]);
        assert!(p.required_permissions("run").is_some());
        assert!(p.required_permissions("test").is_some());
        assert!(p.required_permissions("lint").is_some());
        assert!(p.required_permissions("deploy").is_none());
    }

    // -- Command validation --

    #[test]
    fn validate_allowed_commands() {
        let allowed = default_allowed_commands();
        assert!(validate_build_command("cargo test", &allowed).is_ok());
        assert!(validate_build_command("cargo build --release", &allowed).is_ok());
        assert!(validate_build_command("npm test", &allowed).is_ok());
        assert!(validate_build_command("pytest -v", &allowed).is_ok());
        assert!(validate_build_command("make clean", &allowed).is_ok());
    }

    #[test]
    fn validate_disallowed_commands() {
        let allowed = default_allowed_commands();
        assert!(validate_build_command("rm -rf /", &allowed).is_err());
        assert!(validate_build_command("curl evil.com", &allowed).is_err());
        assert!(validate_build_command("sudo apt install", &allowed).is_err());
    }

    #[test]
    fn validate_empty_allowlist_allows_all() {
        assert!(validate_build_command("anything goes", &[]).is_ok());
    }

    // -- Output parsers --

    #[test]
    fn parse_cargo_test_success() {
        let output = r#"
running 5 tests
test tests::test_one ... ok
test tests::test_two ... ok
test tests::test_three ... ok
test tests::test_four ... ok
test tests::test_five ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
"#;
        let summary = parse_cargo_test_output(output).unwrap();
        assert_eq!(summary.passed, 5);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.total, 5);
    }

    #[test]
    fn parse_cargo_test_with_failures() {
        let output = r#"
running 3 tests
test tests::test_one ... ok
test tests::test_two ... FAILED
--- tests::test_two FAILED
test tests::test_three ... ok

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
"#;
        let summary = parse_cargo_test_output(output).unwrap();
        assert_eq!(summary.passed, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(summary.failures[0].name, "tests::test_two");
    }

    #[test]
    fn parse_pytest_success() {
        let output = "========================= 10 passed in 0.52s =========================";
        let summary = parse_pytest_output(output).unwrap();
        assert_eq!(summary.passed, 10);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn parse_pytest_with_failures() {
        let output = r#"
FAILED tests/test_auth.py::test_login - AssertionError: expected 401
FAILED tests/test_api.py::test_create - KeyError: 'name'
============= 3 passed, 2 failed in 1.23s =============
"#;
        let summary = parse_pytest_output(output).unwrap();
        assert_eq!(summary.passed, 3);
        assert_eq!(summary.failed, 2);
        assert_eq!(summary.failures.len(), 2);
        assert!(summary.failures[0].name.contains("test_login"));
    }

    #[test]
    fn parse_no_test_output() {
        let output = "Build completed successfully.";
        assert!(parse_cargo_test_output(output).is_none());
        assert!(parse_pytest_output(output).is_none());
    }

    // -- Ecosystem detection --

    #[test]
    fn detect_rust_ecosystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_ecosystem(tmp.path()), "rust");
    }

    #[test]
    fn detect_nodejs_ecosystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect_ecosystem(tmp.path()), "nodejs");
    }

    #[test]
    fn detect_python_ecosystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("pyproject.toml"), "").unwrap();
        assert_eq!(detect_ecosystem(tmp.path()), "python");
    }

    #[test]
    fn detect_unknown_ecosystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(detect_ecosystem(tmp.path()), "unknown");
    }

    // -- Action tests --

    #[tokio::test]
    async fn run_echo_command() {
        let p = BuildProvider::new(BuildConfig {
            allowed_commands: vec![], // Empty = allow all
            ..Default::default()
        });
        let ctx = make_context();

        let result = p
            .execute(
                "run",
                json!({"command": "echo build-ok", "working_dir": "/tmp"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(result.output["exit_code"], 0);
        assert!(result.output["output"]
            .as_str()
            .unwrap()
            .contains("build-ok"));
    }

    #[tokio::test]
    async fn run_failing_command() {
        let p = BuildProvider::new(BuildConfig {
            allowed_commands: vec![],
            ..Default::default()
        });
        let ctx = make_context();

        let result = p
            .execute(
                "run",
                json!({"command": "false", "working_dir": "/tmp"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_ne!(result.output["exit_code"], 0);
        assert_eq!(result.output["status"].as_str().unwrap(), "failed");
    }

    #[tokio::test]
    async fn run_disallowed_command_blocked() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute("run", json!({"command": "rm -rf /tmp/important"}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not on the allowed"));
    }

    #[tokio::test]
    async fn test_auto_detect_unknown() {
        let p = make_provider();
        let ctx = make_context();

        // /tmp has no Cargo.toml, package.json, etc. → unknown ecosystem
        let err = p
            .execute("test", json!({"working_dir": "/tmp"}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("cannot auto-detect"));
    }

    #[tokio::test]
    async fn unknown_action_fails() {
        let p = make_provider();
        let ctx = make_context();

        let err = p.execute("deploy", json!({}), &ctx).await.unwrap_err();
        assert!(format!("{err}").contains("unknown build action"));
    }
}
