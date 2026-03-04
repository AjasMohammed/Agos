# Plan 09 — Core Tools (`agentos-tools` crate)

## Goal

Implement 5 built-in tools that ship with AgentOS Phase 1. Each tool has a TOML manifest and a Rust implementation behind the `AgentTool` trait. In Phase 1, tools run in-process (same binary as the kernel) — process-isolated sandboxing is deferred to Phase 2.

## Dependencies

- `agentos-types`
- `agentos-bus`
- `agentos-capability`
- `serde`, `serde_json`
- `tokio` (fs operations)
- `toml` (manifest parsing)
- `tracing`

## AgentTool Trait

```rust
// In src/traits.rs
use agentos_types::*;
use async_trait::async_trait;

/// Every tool implements this trait.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// The tool's name (must match manifest).
    fn name(&self) -> &str;

    /// Execute the tool with the given payload.
    /// The kernel has already validated the capability token and permissions
    /// before calling this method.
    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError>;

    /// Return the permissions this tool requires to operate.
    fn required_permissions(&self) -> Vec<(String, PermissionOp)>;
}

/// Context provided to the tool at execution time.
/// Contains references to kernel resources the tool is allowed to use.
pub struct ToolExecutionContext {
    pub data_dir: PathBuf,        // /opt/agentos/data — where tools read/write files
    pub task_id: TaskID,
    pub trace_id: TraceID,
}
```

## Tool Runner

The `ToolRunner` is responsible for finding the right tool and executing it:

```rust
// In src/runner.rs

pub struct ToolRunner {
    tools: HashMap<String, Box<dyn AgentTool>>,
}

impl ToolRunner {
    pub fn new() -> Self {
        let mut runner = Self { tools: HashMap::new() };
        // Register all built-in tools
        runner.register(Box::new(FileReader::new()));
        runner.register(Box::new(FileWriter::new()));
        runner.register(Box::new(MemorySearch::new()));
        runner.register(Box::new(MemoryWrite::new()));
        runner.register(Box::new(DataParser::new()));
        runner
    }

    pub fn register(&mut self, tool: Box<dyn AgentTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Execute a tool by name. Returns the JSON result.
    pub async fn execute(
        &self,
        tool_name: &str,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let tool = self.tools.get(tool_name)
            .ok_or_else(|| AgentOSError::ToolNotFound(tool_name.to_string()))?;

        tracing::info!(tool = tool_name, task_id = %context.task_id, "Executing tool");

        let start = std::time::Instant::now();
        let result = tool.execute(payload, context).await;
        let duration = start.elapsed();

        tracing::info!(tool = tool_name, duration_ms = duration.as_millis() as u64, "Tool execution completed");

        result
    }

    /// Get the list of all registered tools (for system prompt).
    pub fn list_tools(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Get the required permissions for a given tool.
    pub fn get_required_permissions(&self, tool_name: &str) -> Option<Vec<(String, PermissionOp)>> {
        self.tools.get(tool_name).map(|t| t.required_permissions())
    }
}
```

## Tool Manifest Loader

```rust
// In src/loader.rs

/// Load a ToolManifest from a TOML file.
pub fn load_manifest(path: &Path) -> Result<ToolManifest, AgentOSError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| AgentOSError::ToolNotFound(format!("Cannot read manifest {:?}: {}", path, e)))?;

    toml::from_str(&content)
        .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid manifest {:?}: {}", path, e)))
}

/// Load all manifests from a directory.
pub fn load_all_manifests(dir: &Path) -> Result<Vec<ToolManifest>, AgentOSError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "toml") {
            manifests.push(load_manifest(&path)?);
        }
    }
    Ok(manifests)
}
```

---

## Tool 1: `file-reader`

### Manifest (`tools/core/file-reader.toml`)

```toml
[manifest]
name        = "file-reader"
version     = "1.0.0"
description = "Reads files from the data directory and returns their content as text or structured data"
author      = "agentos-core"

[capabilities_required]
permissions = ["fs.user_data:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "FileReadIntent"
output = "FileContent"

[sandbox]
network       = false
fs_write      = false
max_memory_mb = 64
max_cpu_ms    = 5000
```

### Implementation

```rust
// In src/file_reader.rs

pub struct FileReader;

impl FileReader {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl AgentTool for FileReader {
    fn name(&self) -> &str { "file-reader" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let path_str = payload.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "file-reader requires 'path' field".into()
            ))?;

        // SECURITY: resolve path relative to data_dir only. Prevent path traversal.
        let requested_path = Path::new(path_str);
        let resolved = if requested_path.is_absolute() {
            // Strip leading / and resolve relative to data_dir
            let stripped = requested_path.strip_prefix("/").unwrap_or(requested_path);
            context.data_dir.join(stripped)
        } else {
            context.data_dir.join(requested_path)
        };

        // Canonicalize and verify it's within data_dir
        let canonical = resolved.canonicalize()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("File not found: {} ({})", path_str, e),
            })?;

        if !canonical.starts_with(&context.data_dir) {
            return Err(AgentOSError::PermissionDenied {
                resource: "fs.user_data".into(),
                operation: format!("Path traversal denied: {}", path_str),
            });
        }

        // Read the file
        let content = tokio::fs::read_to_string(&canonical).await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "file-reader".into(),
                reason: format!("Cannot read {}: {}", path_str, e),
            })?;

        let metadata = tokio::fs::metadata(&canonical).await.ok();
        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

        Ok(serde_json::json!({
            "path": path_str,
            "content": content,
            "size_bytes": size,
            "content_type": "text",
        }))
    }
}
```

---

## Tool 2: `file-writer`

### Manifest (`tools/core/file-writer.toml`)

```toml
[manifest]
name        = "file-writer"
version     = "1.0.0"
description = "Writes content to files in the data directory"
author      = "agentos-core"

[capabilities_required]
permissions = ["fs.user_data:w"]

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "FileWriteIntent"
output = "WriteResult"

[sandbox]
network       = false
fs_write      = true
max_memory_mb = 64
max_cpu_ms    = 5000
```

### Implementation

```rust
// In src/file_writer.rs

pub struct FileWriter;

impl FileWriter {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl AgentTool for FileWriter {
    fn name(&self) -> &str { "file-writer" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("fs.user_data".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let path_str = payload.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "file-writer requires 'path' field".into()
            ))?;

        let content = payload.get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "file-writer requires 'content' field".into()
            ))?;

        let append = payload.get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // SECURITY: same path traversal protection as file-reader
        let resolved = context.data_dir.join(
            Path::new(path_str).strip_prefix("/").unwrap_or(Path::new(path_str))
        );

        // Create parent directories if needed
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Cannot create directory: {}", e),
                })?;
        }

        if append {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .create(true).append(true).open(&resolved).await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Cannot open for append: {}", e),
                })?;
            file.write_all(content.as_bytes()).await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Write failed: {}", e),
                })?;
        } else {
            tokio::fs::write(&resolved, content).await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "file-writer".into(),
                    reason: format!("Write failed: {}", e),
                })?;
        }

        Ok(serde_json::json!({
            "path": path_str,
            "bytes_written": content.len(),
            "mode": if append { "append" } else { "overwrite" },
            "success": true,
        }))
    }
}
```

---

## Tool 3: `memory-search`

### Manifest (`tools/core/memory-search.toml`)

```toml
[manifest]
name        = "memory-search"
version     = "1.0.0"
description = "Search semantic memory for relevant past knowledge by keyword or natural language query"
author      = "agentos-core"

[capabilities_required]
permissions = ["memory.semantic:r"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "MemorySearchIntent"
output = "MemorySearchResult"

[sandbox]
network       = false
fs_write      = false
max_memory_mb = 128
max_cpu_ms    = 10000
```

### Implementation (Phase 1 — Simple SQLite FTS)

In Phase 1, semantic memory is implemented as **SQLite FTS5** (Full-Text Search) rather than a full vector store. This is much simpler to implement and still provides useful keyword-based memory search. Vector embeddings can be added in Phase 2.

```rust
// In src/memory_search.rs
use rusqlite::Connection;

pub struct MemorySearch {
    db_path: PathBuf,
}

impl MemorySearch {
    pub fn new(data_dir: &Path) -> Self {
        let db_path = data_dir.join("semantic_memory.db");
        // Initialize the FTS5 table if it doesn't exist
        Self::init_db(&db_path).ok();
        Self { db_path }
    }

    fn init_db(path: &Path) -> Result<(), AgentOSError> {
        let conn = Connection::open(path)?;
        conn.execute_batch("
            CREATE VIRTUAL TABLE IF NOT EXISTS memory USING fts5(
                content,
                source,
                tags,
                created_at
            );
        ")?;
        Ok(())
    }
}

#[async_trait]
impl AgentTool for MemorySearch {
    fn name(&self) -> &str { "memory-search" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Read)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload.get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "memory-search requires 'query' field".into()
            ))?;

        let limit = payload.get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let db_path = self.db_path.clone();
        let query_owned = query.to_string();

        // Run SQLite query on a blocking thread (rusqlite is not async)
        let results = tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path)?;
            let mut stmt = conn.prepare(
                "SELECT content, source, tags, created_at, rank
                 FROM memory
                 WHERE memory MATCH ?1
                 ORDER BY rank
                 LIMIT ?2"
            )?;

            let rows: Vec<serde_json::Value> = stmt.query_map(
                rusqlite::params![&query_owned, limit],
                |row| {
                    Ok(serde_json::json!({
                        "content": row.get::<_, String>(0)?,
                        "source": row.get::<_, String>(1)?,
                        "tags": row.get::<_, String>(2)?,
                        "created_at": row.get::<_, String>(3)?,
                    }))
                },
            )?.filter_map(|r| r.ok()).collect();

            Ok::<_, AgentOSError>(rows)
        }).await.map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "memory-search".into(),
            reason: format!("Task join error: {}", e),
        })??;

        Ok(serde_json::json!({
            "query": query,
            "results": results,
            "count": results.len(),
        }))
    }
}
```

---

## Tool 4: `memory-write`

### Manifest (`tools/core/memory-write.toml`)

```toml
[manifest]
name        = "memory-write"
version     = "1.0.0"
description = "Write new knowledge entries to semantic memory for long-term recall"
author      = "agentos-core"

[capabilities_required]
permissions = ["memory.semantic:w"]

[capabilities_provided]
outputs = ["status"]

[intent_schema]
input  = "MemoryWriteIntent"
output = "WriteResult"

[sandbox]
network       = false
fs_write      = false
max_memory_mb = 64
max_cpu_ms    = 5000
```

### Implementation

```rust
// In src/memory_write.rs

pub struct MemoryWrite {
    db_path: PathBuf,
}

impl MemoryWrite {
    pub fn new(data_dir: &Path) -> Self {
        Self { db_path: data_dir.join("semantic_memory.db") }
    }
}

#[async_trait]
impl AgentTool for MemoryWrite {
    fn name(&self) -> &str { "memory-write" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("memory.semantic".to_string(), PermissionOp::Write)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let content = payload.get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "memory-write requires 'content' field".into()
            ))?;

        let source = payload.get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");

        let tags = payload.get("tags")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let db_path = self.db_path.clone();
        let content = content.to_string();
        let source = source.to_string();
        let tags = tags.to_string();
        let now = chrono::Utc::now().to_rfc3339();

        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path)?;
            conn.execute(
                "INSERT INTO memory (content, source, tags, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![&content, &source, &tags, &now],
            )?;
            Ok::<_, AgentOSError>(())
        }).await.map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "memory-write".into(),
            reason: format!("Task join error: {}", e),
        })??;

        Ok(serde_json::json!({
            "success": true,
            "message": "Memory entry stored successfully",
        }))
    }
}
```

---

## Tool 5: `data-parser`

### Manifest (`tools/core/data-parser.toml`)

```toml
[manifest]
name        = "data-parser"
version     = "1.0.0"
description = "Parse structured data formats (JSON, CSV, TOML, Markdown) and return structured content"
author      = "agentos-core"

[capabilities_required]
permissions = []

[capabilities_provided]
outputs = ["content.structured"]

[intent_schema]
input  = "DataParseIntent"
output = "ParsedData"

[sandbox]
network       = false
fs_write      = false
max_memory_mb = 128
max_cpu_ms    = 10000
```

### Implementation

```rust
// In src/data_parser.rs

pub struct DataParser;

impl DataParser {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl AgentTool for DataParser {
    fn name(&self) -> &str { "data-parser" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![] // no permissions needed — operates on provided data only
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let data = payload.get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "data-parser requires 'data' field (string)".into()
            ))?;

        let format = payload.get("format")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "data-parser requires 'format' field (json|csv|toml)".into()
            ))?;

        let parsed = match format.to_lowercase().as_str() {
            "json" => {
                let value: serde_json::Value = serde_json::from_str(data)
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("Invalid JSON: {}", e),
                    })?;
                value
            }
            "csv" => {
                // Parse CSV into array of objects
                let mut reader = csv::ReaderBuilder::new()
                    .has_headers(true)
                    .from_reader(data.as_bytes());

                let headers: Vec<String> = reader.headers()
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("Invalid CSV headers: {}", e),
                    })?
                    .iter()
                    .map(|h| h.to_string())
                    .collect();

                let mut rows = Vec::new();
                for record in reader.records() {
                    let record = record.map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("Invalid CSV row: {}", e),
                    })?;
                    let mut row = serde_json::Map::new();
                    for (i, field) in record.iter().enumerate() {
                        if let Some(header) = headers.get(i) {
                            row.insert(header.clone(), serde_json::Value::String(field.to_string()));
                        }
                    }
                    rows.push(serde_json::Value::Object(row));
                }

                serde_json::json!({
                    "headers": headers,
                    "rows": rows,
                    "row_count": rows.len(),
                })
            }
            "toml" => {
                let value: toml::Value = toml::from_str(data)
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("Invalid TOML: {}", e),
                    })?;
                serde_json::to_value(value).unwrap_or(serde_json::json!(null))
            }
            other => {
                return Err(AgentOSError::SchemaValidation(
                    format!("Unsupported format: '{}'. Supported: json, csv, toml", other)
                ));
            }
        };

        Ok(serde_json::json!({
            "format": format,
            "parsed": parsed,
        }))
    }
}
```

**Additional dependency needed for CSV**: Add `csv = "1"` to the workspace `Cargo.toml` and `agentos-tools/Cargo.toml`.

---

## Module Exports

```rust
// src/lib.rs
pub mod traits;
pub mod loader;
pub mod runner;
pub mod file_reader;
pub mod file_writer;
pub mod memory_search;
pub mod memory_write;
pub mod data_parser;

pub use traits::{AgentTool, ToolExecutionContext};
pub use runner::ToolRunner;
pub use loader::{load_manifest, load_all_manifests};
```

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_context(data_dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
        }
    }

    #[tokio::test]
    async fn test_file_reader_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "Hello, AgentOS!").unwrap();

        let tool = FileReader::new();
        let result = tool.execute(
            serde_json::json!({"path": "test.txt"}),
            make_context(dir.path()),
        ).await.unwrap();

        assert_eq!(result["content"], "Hello, AgentOS!");
        assert_eq!(result["size_bytes"], 15);
    }

    #[tokio::test]
    async fn test_file_reader_path_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        let tool = FileReader::new();

        let result = tool.execute(
            serde_json::json!({"path": "../../etc/passwd"}),
            make_context(dir.path()),
        ).await;

        assert!(result.is_err()); // Should be PermissionDenied or not found
    }

    #[tokio::test]
    async fn test_file_writer_basic() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriter::new();

        tool.execute(
            serde_json::json!({"path": "output.txt", "content": "Hello!"}),
            make_context(dir.path()),
        ).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("output.txt")).unwrap();
        assert_eq!(content, "Hello!");
    }

    #[tokio::test]
    async fn test_file_writer_creates_subdirectories() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriter::new();

        tool.execute(
            serde_json::json!({"path": "subdir/nested/file.txt", "content": "Deep!"}),
            make_context(dir.path()),
        ).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("subdir/nested/file.txt")).unwrap();
        assert_eq!(content, "Deep!");
    }

    #[tokio::test]
    async fn test_data_parser_json() {
        let tool = DataParser::new();
        let result = tool.execute(
            serde_json::json!({
                "data": r#"{"name": "test", "value": 42}"#,
                "format": "json"
            }),
            make_context(Path::new("/tmp")),
        ).await.unwrap();

        assert_eq!(result["parsed"]["name"], "test");
        assert_eq!(result["parsed"]["value"], 42);
    }

    #[tokio::test]
    async fn test_data_parser_csv() {
        let tool = DataParser::new();
        let result = tool.execute(
            serde_json::json!({
                "data": "name,age\nAlice,30\nBob,25",
                "format": "csv"
            }),
            make_context(Path::new("/tmp")),
        ).await.unwrap();

        assert_eq!(result["parsed"]["row_count"], 2);
        assert_eq!(result["parsed"]["rows"][0]["name"], "Alice");
    }

    #[tokio::test]
    async fn test_memory_write_and_search() {
        let dir = TempDir::new().unwrap();
        let write_tool = MemoryWrite::new(dir.path());
        let search_tool = MemorySearch::new(dir.path());
        let ctx = make_context(dir.path());

        // Write a memory entry
        write_tool.execute(
            serde_json::json!({
                "content": "The capital of France is Paris",
                "source": "test",
                "tags": "geography facts"
            }),
            ctx.clone(),
        ).await.unwrap();

        // Search for it
        let result = search_tool.execute(
            serde_json::json!({"query": "capital France", "limit": 5}),
            ctx,
        ).await.unwrap();

        assert!(result["count"].as_u64().unwrap() > 0);
        assert!(result["results"][0]["content"].as_str().unwrap().contains("Paris"));
    }

    #[test]
    fn test_tool_runner_lists_all_built_in_tools() {
        let runner = ToolRunner::new();
        let tools = runner.list_tools();
        assert!(tools.contains(&"file-reader".to_string()));
        assert!(tools.contains(&"file-writer".to_string()));
        assert!(tools.contains(&"memory-search".to_string()));
        assert!(tools.contains(&"memory-write".to_string()));
        assert!(tools.contains(&"data-parser".to_string()));
        assert_eq!(tools.len(), 5);
    }
}
```

## Verification

```bash
cargo test -p agentos-tools
```
