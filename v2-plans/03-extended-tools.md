# Plan 03 — Extended Tools

## Goal

Implement 5 new tools that extend AgentOS's capabilities beyond file and memory operations. These tools give agents the ability to make HTTP requests, inspect the system, read logs, execute shell commands, and run code snippets — all under capability-gated sandboxing.

## Dependencies

- `agentos-types`, `agentos-tools` (existing)
- `agentos-sandbox` (from Plan 01 — seccomp profiles)
- `sysinfo` — cross-platform system information
- `reqwest` — HTTP client (already in workspace)
- `tokio::process` — async command execution

## New Dependency

```toml
# Add to workspace Cargo.toml
sysinfo = "0.32"
```

---

## Tool 6: `http-client`

### Manifest (`tools/core/http-client.toml`)

```toml
[manifest]
name        = "http-client"
version     = "1.0.0"
description = "Make outbound HTTP requests (GET, POST, PUT, DELETE) and return responses"
author      = "agentos-core"

[capabilities_required]
permissions = ["network.outbound:x"]

[capabilities_provided]
outputs = ["content.text", "content.structured"]

[intent_schema]
input  = "HttpRequestIntent"
output = "HttpResponse"

[sandbox]
network       = true
fs_write      = false
max_memory_mb = 128
max_cpu_ms    = 30000
```

### Implementation

```rust
// In src/http_client.rs

pub struct HttpClient {
    client: reqwest::Client,
}

impl HttpClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent("AgentOS/0.2 http-client")
                .build()
                .expect("Failed to build HTTP client"),
        }
    }
}

#[async_trait]
impl AgentTool for HttpClient {
    fn name(&self) -> &str { "http-client" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.outbound".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let url = payload.get("url").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation("http-client requires 'url' field".into()))?;

        let method = payload.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
        let body = payload.get("body").and_then(|v| v.as_str());
        let headers: HashMap<String, String> = payload.get("headers")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let mut request = match method.to_uppercase().as_str() {
            "GET" => self.client.get(url),
            "POST" => self.client.post(url),
            "PUT" => self.client.put(url),
            "DELETE" => self.client.delete(url),
            "PATCH" => self.client.patch(url),
            other => return Err(AgentOSError::SchemaValidation(
                format!("Unsupported HTTP method: {}", other)
            )),
        };

        for (key, value) in &headers {
            request = request.header(key.as_str(), value.as_str());
        }

        if let Some(body) = body {
            request = request.body(body.to_string());
        }

        let response = request.send().await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "http-client".into(),
                reason: format!("Request failed: {}", e),
            })?;

        let status = response.status().as_u16();
        let response_headers: HashMap<String, String> = response.headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body_text = response.text().await.unwrap_or_default();

        // Truncate very large responses
        let truncated = body_text.len() > 100_000;
        let body_display = if truncated {
            format!("{}... [TRUNCATED: {} bytes total]", &body_text[..100_000], body_text.len())
        } else {
            body_text
        };

        Ok(serde_json::json!({
            "status": status,
            "headers": response_headers,
            "body": body_display,
            "truncated": truncated,
        }))
    }
}
```

---

## Tool 7: `sys-monitor`

### Manifest (`tools/core/sys-monitor.toml`)

```toml
[manifest]
name        = "sys-monitor"
version     = "1.0.0"
description = "Read system resource usage: CPU, memory, disk, and process information"
author      = "agentos-core"

[capabilities_required]
permissions = ["hardware.system:r"]

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "SystemMonitorIntent"
output = "SystemInfo"

[sandbox]
network       = false
fs_write      = false
max_memory_mb = 64
max_cpu_ms    = 5000
```

### Implementation

```rust
// In src/sys_monitor.rs
use sysinfo::System;

pub struct SysMonitor;

impl SysMonitor {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl AgentTool for SysMonitor {
    fn name(&self) -> &str { "sys-monitor" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("hardware.system".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload.get("query").and_then(|v| v.as_str()).unwrap_or("overview");

        let mut sys = System::new_all();
        sys.refresh_all();

        match query {
            "overview" => Ok(serde_json::json!({
                "cpu_usage_percent": sys.global_cpu_usage(),
                "cpu_count": sys.cpus().len(),
                "total_memory_mb": sys.total_memory() / 1024 / 1024,
                "used_memory_mb": sys.used_memory() / 1024 / 1024,
                "available_memory_mb": sys.available_memory() / 1024 / 1024,
                "total_swap_mb": sys.total_swap() / 1024 / 1024,
                "used_swap_mb": sys.used_swap() / 1024 / 1024,
                "system_name": System::name(),
                "kernel_version": System::kernel_version(),
                "os_version": System::os_version(),
                "host_name": System::host_name(),
                "uptime_secs": System::uptime(),
            })),
            "processes" => {
                let limit = payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                let mut procs: Vec<_> = sys.processes().values()
                    .map(|p| serde_json::json!({
                        "pid": p.pid().as_u32(),
                        "name": p.name().to_string_lossy(),
                        "cpu_percent": p.cpu_usage(),
                        "memory_mb": p.memory() / 1024 / 1024,
                        "status": format!("{:?}", p.status()),
                    }))
                    .collect();
                procs.truncate(limit);
                Ok(serde_json::json!({
                    "processes": procs,
                    "total_processes": sys.processes().len(),
                }))
            },
            "disks" => {
                let disks: Vec<_> = sysinfo::Disks::new_with_refreshed_list()
                    .iter()
                    .map(|d| serde_json::json!({
                        "name": d.name().to_string_lossy(),
                        "mount_point": d.mount_point().to_string_lossy(),
                        "total_gb": d.total_space() / 1_073_741_824,
                        "available_gb": d.available_space() / 1_073_741_824,
                        "file_system": d.file_system().to_string_lossy(),
                    }))
                    .collect();
                Ok(serde_json::json!({ "disks": disks }))
            },
            other => Err(AgentOSError::SchemaValidation(
                format!("Unknown query type '{}'. Use: overview, processes, disks", other)
            )),
        }
    }
}
```

---

## Tool 8: `log-reader`

### Manifest (`tools/core/log-reader.toml`)

```toml
[manifest]
name        = "log-reader"
version     = "1.0.0"
description = "Read application and system log files with filtering and tail support"
author      = "agentos-core"

[capabilities_required]
permissions = ["fs.app_logs:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "LogReadIntent"
output = "LogContent"

[sandbox]
network       = false
fs_write      = false
max_memory_mb = 128
max_cpu_ms    = 10000
```

### Implementation

Reads log files from a configured logs directory. Supports `tail` (last N lines), `grep` (pattern filter), and `since` (time-based filter).

```rust
// In src/log_reader.rs

pub struct LogReader {
    logs_dir: PathBuf,
}

impl LogReader {
    pub fn new(logs_dir: &Path) -> Self {
        Self { logs_dir: logs_dir.to_path_buf() }
    }
}

#[async_trait]
impl AgentTool for LogReader {
    fn name(&self) -> &str { "log-reader" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.app_logs".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let file = payload.get("file").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation("log-reader requires 'file' field".into()))?;
        let tail = payload.get("tail").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
        let pattern = payload.get("grep").and_then(|v| v.as_str());

        // Path traversal protection — same as file-reader
        let resolved = self.logs_dir.join(Path::new(file).strip_prefix("/").unwrap_or(Path::new(file)));
        let canonical = resolved.canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "log-reader".into(),
                reason: format!("Log file not found: {} ({})", file, e),
            })?;

        if !canonical.starts_with(&self.logs_dir) {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.app_logs".into(),
                operation: format!("Path traversal denied: {}", file),
            });
        }

        let content = tokio::fs::read_to_string(&canonical).await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "log-reader".into(),
                reason: format!("Cannot read log: {}", e),
            })?;

        let mut lines: Vec<&str> = content.lines().collect();

        // Apply grep filter if specified
        if let Some(pattern) = pattern {
            lines.retain(|line| line.contains(pattern));
        }

        // Tail — take last N lines
        let total = lines.len();
        if lines.len() > tail {
            lines = lines[lines.len() - tail..].to_vec();
        }

        Ok(serde_json::json!({
            "file": file,
            "lines": lines,
            "line_count": lines.len(),
            "total_lines": total,
            "truncated": total > tail,
        }))
    }
}
```

---

## Tool 9: `shell-exec`

### Manifest (`tools/core/shell-exec.toml`)

```toml
[manifest]
name        = "shell-exec"
version     = "1.0.0"
description = "Execute shell commands with strict timeout and output capture (RESTRICTED — requires explicit permission)"
author      = "agentos-core"

[capabilities_required]
permissions = ["process.exec:x"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "ShellExecIntent"
output = "ShellExecResult"

[sandbox]
network       = false
fs_write      = true
max_memory_mb = 256
max_cpu_ms    = 30000
```

### Implementation

> [!CAUTION]
> This tool requires explicit `process.exec:x` permission. It is **not granted by default** to any agent.

```rust
// In src/shell_exec.rs
use tokio::process::Command;

pub struct ShellExec;

impl ShellExec {
    pub fn new() -> Self { Self }
}

// COMMAND BLOCKLIST — commands that are always denied
const BLOCKED_COMMANDS: &[&str] = &[
    "rm -rf", "mkfs", "dd if=", "shutdown", "reboot",
    "passwd", "useradd", "userdel", "chmod 777",
    "curl | sh", "wget | sh",
];

#[async_trait]
impl AgentTool for ShellExec {
    fn name(&self) -> &str { "shell-exec" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("process.exec".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let command = payload.get("command").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation("shell-exec requires 'command' field".into()))?;

        let timeout_secs = payload.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(30);

        // Security: check against blocklist
        for blocked in BLOCKED_COMMANDS {
            if command.contains(blocked) {
                return Err(AgentOSError::PermissionDenied {
                    resource: "process.exec".into(),
                    operation: format!("Command blocked by safety filter: {}", blocked),
                });
            }
        }

        let output = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .output()
        ).await
        .map_err(|_| AgentOSError::ToolExecutionFailed {
            tool_name: "shell-exec".into(),
            reason: format!("Command timed out after {}s", timeout_secs),
        })?
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "shell-exec".into(),
            reason: format!("Failed to execute command: {}", e),
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Truncate large outputs
        let max_output = 50_000;
        let stdout_display = if stdout.len() > max_output {
            format!("{}... [TRUNCATED]", &stdout[..max_output])
        } else {
            stdout.to_string()
        };

        Ok(serde_json::json!({
            "command": command,
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": stdout_display,
            "stderr": stderr.to_string(),
            "success": output.status.success(),
        }))
    }
}
```

---

## Tool 10: `code-runner`

### Manifest (`tools/core/code-runner.toml`)

```toml
[manifest]
name        = "code-runner"
version     = "1.0.0"
description = "Execute code snippets in isolation (Python, JavaScript, Bash)"
author      = "agentos-core"

[capabilities_required]
permissions = ["process.exec:x"]

[capabilities_provided]
outputs = ["content.text", "content.structured"]

[intent_schema]
input  = "CodeRunIntent"
output = "CodeRunResult"

[sandbox]
network       = false
fs_write      = true
max_memory_mb = 256
max_cpu_ms    = 60000
```

### Implementation

Writes code to a temp file, executes it with the appropriate interpreter, captures output.

Supported languages: `python`, `javascript` (Node.js), `bash`.

```rust
// In src/code_runner.rs

pub struct CodeRunner;

// Language → interpreter mapping
fn interpreter(language: &str) -> Result<&str, AgentOSError> {
    match language {
        "python" | "python3" => Ok("python3"),
        "javascript" | "js" | "node" => Ok("node"),
        "bash" | "sh" => Ok("bash"),
        other => Err(AgentOSError::SchemaValidation(
            format!("Unsupported language: '{}'. Supported: python, javascript, bash", other)
        )),
    }
}
```

---

## ToolRunner Update

Register the new tools in the `ToolRunner::new()` constructor:

```rust
impl ToolRunner {
    pub fn new(data_dir: &Path) -> Self {
        let mut runner = Self { tools: HashMap::new() };
        // V1 tools
        runner.register(Box::new(FileReader::new()));
        runner.register(Box::new(FileWriter::new()));
        runner.register(Box::new(MemorySearch::new(data_dir)));
        runner.register(Box::new(MemoryWrite::new(data_dir)));
        runner.register(Box::new(DataParser::new()));
        // V2 tools
        runner.register(Box::new(HttpClient::new()));
        runner.register(Box::new(SysMonitor::new()));
        runner.register(Box::new(LogReader::new(&data_dir.join("logs"))));
        runner.register(Box::new(ShellExec::new()));
        runner.register(Box::new(CodeRunner::new()));
        runner
    }
}
```

## Tests

```rust
#[tokio::test]
async fn test_http_client_get() {
    // Use httpbin.org or a local mock server
}

#[tokio::test]
async fn test_sys_monitor_overview() {
    let tool = SysMonitor::new();
    let result = tool.execute(json!({"query": "overview"}), make_context(dir.path())).await.unwrap();
    assert!(result["cpu_count"].as_u64().unwrap() > 0);
    assert!(result["total_memory_mb"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn test_log_reader_tail() {
    // Create a log file, verify tail returns last N lines
}

#[tokio::test]
async fn test_shell_exec_blocked_command() {
    let tool = ShellExec::new();
    let result = tool.execute(json!({"command": "rm -rf /"}), make_context(dir.path())).await;
    assert!(result.is_err()); // Blocked by safety filter
}

#[tokio::test]
async fn test_tool_runner_lists_10_tools() {
    let runner = ToolRunner::new(dir.path());
    assert!(runner.list_tools().len() >= 10);
}
```

## Verification

```bash
cargo test -p agentos-tools
```
