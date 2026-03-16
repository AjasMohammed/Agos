pub mod agent_message;
pub mod archival_insert;
pub mod archival_search;
pub mod data_parser;
pub mod file_lock;
pub mod file_reader;
pub mod file_writer;
pub mod hardware_info;
pub mod http_client;
pub mod loader;
pub mod log_reader;
pub mod memory_block_delete;
pub mod memory_block_list;
pub mod memory_block_read;
pub mod memory_block_write;
pub mod memory_search;
pub mod memory_write;
pub mod network_monitor;
pub mod process_manager;
pub mod runner;
pub mod sanitize;
pub mod shell_exec;
pub mod signing;
pub mod sys_monitor;
pub mod task_delegate;
pub mod traits;

pub use agent_message::AgentMessageTool;
pub use archival_insert::ArchivalInsert;
pub use archival_search::ArchivalSearch;
pub use data_parser::DataParser;
pub use file_lock::{FileLockRegistry, WriteLockGuard};
pub use file_reader::FileReader;
pub use file_writer::FileWriter;
pub use hardware_info::HardwareInfoTool;
pub use http_client::HttpClientTool;
pub use loader::{load_all_manifests, load_manifest};
pub use log_reader::LogReaderTool;
pub use memory_block_delete::MemoryBlockDeleteTool;
pub use memory_block_list::MemoryBlockListTool;
pub use memory_block_read::MemoryBlockReadTool;
pub use memory_block_write::MemoryBlockWriteTool;
pub use memory_search::MemorySearch;
pub use memory_write::MemoryWrite;
pub use network_monitor::NetworkMonitorTool;
pub use process_manager::ProcessManagerTool;
pub use runner::ToolRunner;
pub use shell_exec::ShellExec;
pub use signing::{pubkey_hex_from_seed, sign_manifest, signing_payload, verify_manifest};
pub use sys_monitor::SysMonitorTool;
pub use task_delegate::TaskDelegate;
pub use traits::{AgentTool, ToolExecutionContext};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::ToolExecutionContext;
    use agentos_types::*;

    #[tokio::test]
    async fn test_agent_message_tool() {
        let tool = crate::agent_message::AgentMessageTool::new();
        let payload = serde_json::json!({
            "to": "analyst",
            "content": "Analyze the log"
        });

        let ctx = ToolExecutionContext {
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            data_dir: std::path::PathBuf::from("/tmp"),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
        };

        let result = tool.execute(payload, ctx).await.unwrap();
        assert_eq!(
            result.get("_kernel_action").unwrap().as_str().unwrap(),
            "send_agent_message"
        );
        assert_eq!(result.get("to").unwrap().as_str().unwrap(), "analyst");
        assert_eq!(
            result.get("content").unwrap().as_str().unwrap(),
            "Analyze the log"
        );
    }

    #[tokio::test]
    async fn test_task_delegate_tool() {
        let tool = crate::task_delegate::TaskDelegate::new();
        let payload = serde_json::json!({
            "agent": "researcher",
            "task": "Find top 10 error sources",
            "priority": 8
        });

        let ctx = ToolExecutionContext {
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            data_dir: std::path::PathBuf::from("/tmp"),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
        };

        let result = tool.execute(payload, ctx).await.unwrap();
        assert_eq!(
            result.get("_kernel_action").unwrap().as_str().unwrap(),
            "delegate_task"
        );
        assert_eq!(
            result.get("target_agent").unwrap().as_str().unwrap(),
            "researcher"
        );
        assert_eq!(
            result.get("task").unwrap().as_str().unwrap(),
            "Find top 10 error sources"
        );
        assert_eq!(result.get("priority").unwrap().as_u64().unwrap(), 8);
    }
    use agentos_memory::{Embedder, EpisodicStore, SemanticStore};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_context_with_permissions(
        data_dir: &Path,
        permissions: PermissionSet,
    ) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions,
            vault: None,
            hal: None,
            file_lock_registry: None,
        }
    }

    fn make_context_with_lock_registry(
        data_dir: &Path,
        registry: std::sync::Arc<crate::file_lock::FileLockRegistry>,
    ) -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant("fs.user_data".to_string(), true, true, false, None);
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions,
            vault: None,
            hal: None,
            file_lock_registry: Some(registry),
        }
    }

    fn make_context(data_dir: &Path) -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant("memory.semantic".to_string(), true, true, false, None);
        permissions.grant("memory.episodic".to_string(), true, true, false, None);
        make_context_with_permissions(data_dir, permissions)
    }

    fn make_memory_stores(data_dir: &Path) -> (Arc<SemanticStore>, Arc<EpisodicStore>) {
        let embedder = Arc::new(Embedder::new().unwrap());
        let semantic = Arc::new(SemanticStore::open_with_embedder(data_dir, embedder).unwrap());
        let episodic = Arc::new(EpisodicStore::open(data_dir).unwrap());
        (semantic, episodic)
    }

    #[tokio::test]
    async fn test_file_reader_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "Hello, AgentOS!").unwrap();

        let tool = FileReader::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "test.txt"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(result["content"], "Hello, AgentOS!");
        assert_eq!(result["size_bytes"], 15);
    }

    #[tokio::test]
    async fn test_file_reader_path_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        let tool = FileReader::new();

        let result = tool
            .execute(
                serde_json::json!({"path": "../../etc/passwd"}),
                make_context(dir.path()),
            )
            .await;

        assert!(result.is_err()); // Should be blocked
    }

    #[tokio::test]
    async fn test_file_writer_path_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriter::new();

        let result = tool
            .execute(
                serde_json::json!({"path": "../../etc/cron.d/evil", "content": "malicious"}),
                make_context(dir.path()),
            )
            .await;

        assert!(result.is_err()); // Should be blocked
    }

    #[tokio::test]
    async fn test_file_writer_basic() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriter::new();

        tool.execute(
            serde_json::json!({"path": "output.txt", "content": "Hello!"}),
            make_context(dir.path()),
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join("output.txt")).unwrap();
        assert_eq!(content, "Hello!");
    }

    #[tokio::test]
    async fn test_file_writer_creates_subdirectories() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriter::new();

        tool.execute(
            serde_json::json!({"path": "subdir/nested/file.txt", "content": "Deep write!"}),
            make_context(dir.path()),
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join("subdir/nested/file.txt")).unwrap();
        assert_eq!(content, "Deep write!");
    }

    #[tokio::test]
    async fn test_data_parser_json() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();

        let result = tool
            .execute(
                serde_json::json!({"data": r#"{"name":"Alice","age":30}"#, "format": "json"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(result["format"], "json");
        assert_eq!(result["parsed"]["name"], "Alice");
        assert_eq!(result["parsed"]["age"], 30);
    }

    #[tokio::test]
    async fn test_data_parser_csv() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();

        let csv_data = "name,age\nAlice,30\nBob,25";
        let result = tool
            .execute(
                serde_json::json!({"data": csv_data, "format": "csv"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(result["format"], "csv");
        assert_eq!(result["parsed"]["row_count"], 2);
        assert_eq!(result["parsed"]["headers"][0], "name");
        assert_eq!(result["parsed"]["rows"][0]["name"], "Alice");
        assert_eq!(result["parsed"]["rows"][1]["age"], "25");
    }

    #[tokio::test]
    async fn test_memory_write_and_search() {
        let dir = TempDir::new().unwrap();
        let ctx = make_context(dir.path());
        let (semantic, episodic) = make_memory_stores(dir.path());

        let search_tool = MemorySearch::new(semantic.clone(), episodic.clone());
        let write_tool = MemoryWrite::new(semantic, episodic);

        // Write a memory entry with embeddings
        let write_result = write_tool
            .execute(
                serde_json::json!({"content": "Q1 revenue was 2.5 million dollars", "key": "q1-revenue", "tags": "revenue,q1"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert_eq!(write_result["success"], true);
        assert_eq!(write_result["scope"], "semantic");
        assert!(write_result["id"].is_string());

        // Search for it using hybrid vector + FTS search
        let search_result = search_tool
            .execute(
                serde_json::json!({"query": "revenue earnings", "top_k": 5}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(search_result["count"], 1);
        assert!(search_result["results"][0]["content"]
            .as_str()
            .unwrap()
            .contains("2.5 million"));
        // Should have semantic score from vector search
        assert!(
            search_result["results"][0]["semantic_score"]
                .as_f64()
                .unwrap()
                > 0.0
        );
    }

    #[tokio::test]
    async fn test_episodic_memory_write_and_search() {
        let dir = TempDir::new().unwrap();
        let ctx = make_context(dir.path());
        let (semantic, episodic) = make_memory_stores(dir.path());

        let search_tool = MemorySearch::new(semantic.clone(), episodic.clone());
        let write_tool = MemoryWrite::new(semantic, episodic);

        // Write a memory entry with episodic scope
        let write_result = write_tool
            .execute(
                serde_json::json!({"content": "Episodic event: agent booted up", "scope": "episodic", "summary": "Agent boot event"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert_eq!(write_result["success"], true);
        assert_eq!(write_result["scope"], "episodic");

        // Search for it with episodic scope (FTS5 search)
        let search_result = search_tool
            .execute(
                serde_json::json!({"query": "booted", "limit": 5, "scope": "episodic"}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(search_result["count"], 1);
        assert_eq!(search_result["results"][0]["scope"], "episodic");
        assert!(search_result["results"][0]["content"]
            .as_str()
            .unwrap()
            .contains("booted up"));
    }

    #[tokio::test]
    async fn test_memory_search_denies_without_semantic_permission() {
        let dir = TempDir::new().unwrap();
        let (semantic, episodic) = make_memory_stores(dir.path());
        let search_tool = MemorySearch::new(semantic.clone(), episodic.clone());
        let write_tool = MemoryWrite::new(semantic, episodic);

        let mut write_perms = PermissionSet::new();
        write_perms.grant("memory.semantic".to_string(), true, true, false, None);
        write_perms.grant("memory.episodic".to_string(), true, true, false, None);
        let write_ctx = make_context_with_permissions(dir.path(), write_perms);
        write_tool
            .execute(
                serde_json::json!({"content": "Deployment docs", "key": "deploy"}),
                write_ctx,
            )
            .await
            .unwrap();

        let mut read_perms = PermissionSet::new();
        read_perms.grant("memory.episodic".to_string(), true, true, false, None);
        let read_ctx = make_context_with_permissions(dir.path(), read_perms);
        let err = search_tool
            .execute(serde_json::json!({"query": "deployment"}), read_ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn test_episodic_global_search_denies_without_episodic_permission() {
        let dir = TempDir::new().unwrap();
        let (semantic, episodic) = make_memory_stores(dir.path());
        let search_tool = MemorySearch::new(semantic.clone(), episodic.clone());
        let write_tool = MemoryWrite::new(semantic, episodic);

        let mut write_perms = PermissionSet::new();
        write_perms.grant("memory.episodic".to_string(), true, true, false, None);
        write_perms.grant("memory.semantic".to_string(), true, true, false, None);
        let write_ctx = make_context_with_permissions(dir.path(), write_perms.clone());
        write_tool
            .execute(
                serde_json::json!({"content": "agent booted", "scope": "episodic"}),
                write_ctx,
            )
            .await
            .unwrap();

        let mut read_perms = PermissionSet::new();
        read_perms.grant("memory.semantic".to_string(), true, true, false, None);
        let read_ctx = make_context_with_permissions(dir.path(), read_perms);
        let err = search_tool
            .execute(
                serde_json::json!({"query": "booted", "scope": "episodic", "global": true}),
                read_ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn test_tool_runner_lists_all_built_in_tools() {
        let dir = TempDir::new().unwrap();
        let runner = ToolRunner::new(dir.path());
        let tools = runner.list_tools();

        assert!(
            tools.len() >= 5,
            "Expected at least 5 built-in tools, got {}",
            tools.len()
        );
        assert!(tools.contains(&"file-reader".to_string()));
        assert!(tools.contains(&"file-writer".to_string()));
        assert!(tools.contains(&"memory-search".to_string()));
        assert!(tools.contains(&"memory-write".to_string()));
        assert!(tools.contains(&"data-parser".to_string()));
    }

    // ── file-reader pagination ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_file_reader_pagination() {
        let dir = TempDir::new().unwrap();
        let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        std::fs::write(dir.path().join("big.txt"), lines.join("\n")).unwrap();

        let tool = FileReader::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({"path": "big.txt", "offset": 10, "limit": 10}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["returned_lines"], 10);
        assert_eq!(result["offset"], 10);
        assert_eq!(result["total_lines"], 100);
        assert_eq!(result["has_more"], true);
        assert!(result["content"].as_str().unwrap().starts_with("line 10"));
    }

    #[tokio::test]
    async fn test_file_reader_has_more_flag() {
        let dir = TempDir::new().unwrap();
        let content = (0..20)
            .map(|i| format!("L{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("f.txt"), &content).unwrap();

        let tool = FileReader::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(serde_json::json!({"path": "f.txt", "limit": 5}), ctx)
            .await
            .unwrap();

        assert_eq!(result["returned_lines"], 5);
        assert_eq!(result["has_more"], true);
    }

    #[tokio::test]
    async fn test_file_reader_no_more_when_within_limit() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("small.txt"), "a\nb\nc").unwrap();

        let tool = FileReader::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(serde_json::json!({"path": "small.txt", "limit": 500}), ctx)
            .await
            .unwrap();

        assert_eq!(result["has_more"], false);
        assert_eq!(result["total_lines"], 3);
    }

    #[tokio::test]
    async fn test_file_reader_directory_list() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
        std::fs::write(dir.path().join("beta.txt"), "b").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let tool = FileReader::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(serde_json::json!({"path": ".", "mode": "list"}), ctx)
            .await
            .unwrap();

        assert_eq!(result["mode"], "list");
        assert_eq!(result["count"], 3);
        let entries = result["entries"].as_array().unwrap();
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"alpha.txt"));
        assert!(names.contains(&"beta.txt"));
        assert!(names.contains(&"subdir"));
    }

    // ── file-reader lock enforcement ────────────────────────────────────────

    #[tokio::test]
    async fn test_file_reader_blocked_when_locked() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("locked.txt"), "secret").unwrap();

        let registry = std::sync::Arc::new(crate::file_lock::FileLockRegistry::new());
        let locked_path = dir.path().canonicalize().unwrap().join("locked.txt");
        registry
            .try_acquire(&locked_path, AgentID::new(), TaskID::new())
            .unwrap();

        let ctx = make_context_with_lock_registry(dir.path(), registry);
        let tool = FileReader::new();
        let err = tool
            .execute(serde_json::json!({"path": "locked.txt"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::FileLocked { .. }));
    }

    // ── file-writer modes ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_file_writer_create_only_succeeds_on_new_file() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriter::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({"path": "new.txt", "content": "hello", "mode": "create_only"}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["mode"], "create_only");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn test_file_writer_create_only_fails_if_exists() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("exists.txt"), "old").unwrap();

        let tool = FileWriter::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({"path": "exists.txt", "content": "new", "mode": "create_only"}),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
        // Original content must be untouched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("exists.txt")).unwrap(),
            "old"
        );
    }

    #[tokio::test]
    async fn test_file_writer_size_guard() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriter::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let big = "x".repeat(200);
        let err = tool
            .execute(
                serde_json::json!({"path": "out.txt", "content": big, "max_bytes": 100}),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
    }

    // ── file-writer lock enforcement ────────────────────────────────────────

    #[tokio::test]
    async fn test_file_writer_blocked_when_locked() {
        let dir = TempDir::new().unwrap();
        let registry = std::sync::Arc::new(crate::file_lock::FileLockRegistry::new());

        // Pre-acquire the lock as a different agent.
        let locked_path = dir.path().canonicalize().unwrap().join("data.txt");
        registry
            .try_acquire(&locked_path, AgentID::new(), TaskID::new())
            .unwrap();

        let ctx = make_context_with_lock_registry(dir.path(), registry);
        let tool = FileWriter::new();
        let err = tool
            .execute(
                serde_json::json!({"path": "data.txt", "content": "hi"}),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::FileLocked { .. }));
    }

    #[tokio::test]
    async fn test_file_writer_lock_released_after_write() {
        let dir = TempDir::new().unwrap();
        let registry = std::sync::Arc::new(crate::file_lock::FileLockRegistry::new());
        let ctx = make_context_with_lock_registry(dir.path(), registry.clone());

        let tool = FileWriter::new();
        tool.execute(
            serde_json::json!({"path": "out.txt", "content": "hello"}),
            ctx,
        )
        .await
        .unwrap();

        // After write, the lock must be released and the path must be free.
        let resolved = dir.path().canonicalize().unwrap().join("out.txt");
        assert!(
            registry.check(&resolved).is_ok(),
            "Lock should be released after write"
        );
    }
}
