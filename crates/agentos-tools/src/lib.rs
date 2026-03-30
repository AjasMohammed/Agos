pub mod agent_call;
pub mod agent_list;
pub mod agent_manual;
pub mod agent_message;
pub mod agent_self;
pub mod archival_insert;
pub mod archival_search;
pub mod ask_user;
pub mod context_memory_read;
pub mod context_memory_update;
pub mod data_parser;
pub mod datetime;
pub mod episodic_list;
pub mod escalation_status;
pub mod factory;
pub mod file_delete;
pub mod file_diff;
pub mod file_editor;
pub mod file_glob;
pub mod file_grep;
pub mod file_lock;
pub mod file_move;
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
pub mod memory_delete;
pub mod memory_read;
pub mod memory_search;
pub mod memory_stats;
pub mod memory_write;
pub mod network_monitor;
pub mod notify_user;
pub mod procedure_create;
pub mod procedure_delete;
pub mod procedure_list;
pub mod procedure_search;
pub mod process_manager;
pub mod runner;
pub mod sanitize;
pub mod scratch_delete;
pub mod scratch_graph;
pub mod scratch_links;
pub mod scratch_read;
pub mod scratch_search;
pub mod scratch_write;
pub mod shell_exec;
pub mod signing;
pub(crate) mod ssrf;
pub mod sys_monitor;
pub mod task_delegate;
pub mod task_list;
pub mod task_status;
pub mod think;
pub mod traits;
pub mod usb_storage;
pub mod web_fetch;
pub mod workspace;

pub use agent_call::AgentCallTool;
pub use agent_list::AgentListTool;
pub use agent_manual::AgentManualTool;
pub use agent_message::AgentMessageTool;
pub use agent_self::AgentSelfTool;
pub use archival_insert::ArchivalInsert;
pub use archival_search::ArchivalSearch;
pub use ask_user::AskUserTool;
pub use data_parser::DataParser;
pub use datetime::DatetimeTool;
pub use episodic_list::EpisodicList;
pub use factory::{
    build_single_tool, build_single_tool_with_model_cache,
    build_single_tool_with_model_cache_and_weight, tool_category, tool_category_with_weight,
    ToolCategory,
};
pub use file_delete::FileDelete;
pub use file_diff::FileDiff;
pub use file_editor::FileEditor;
pub use file_glob::FileGlob;
pub use file_grep::FileGrep;
pub use file_lock::{FileLockRegistry, WriteLockGuard};
pub use file_move::FileMove;
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
pub use memory_delete::MemoryDelete;
pub use memory_read::MemoryRead;
pub use memory_search::MemorySearch;
pub use memory_stats::MemoryStats;
pub use memory_write::MemoryWrite;
pub use network_monitor::NetworkMonitorTool;
pub use notify_user::NotifyUserTool;
pub use procedure_create::ProcedureCreate;
pub use procedure_delete::ProcedureDelete;
pub use procedure_list::ProcedureList;
pub use procedure_search::ProcedureSearch;
pub use process_manager::ProcessManagerTool;
pub use runner::ToolRunner;
pub use scratch_delete::ScratchDeleteTool;
pub use scratch_graph::ScratchGraphTool;
pub use scratch_links::ScratchLinksTool;
pub use scratch_read::ScratchReadTool;
pub use scratch_search::ScratchSearchTool;
pub use scratch_write::ScratchWriteTool;
pub use shell_exec::ShellExec;
pub use signing::{pubkey_hex_from_seed, sign_manifest, signing_payload, verify_manifest};
pub use sys_monitor::SysMonitorTool;
pub use task_delegate::TaskDelegate;
pub use task_list::TaskListTool;
pub use task_status::TaskStatusTool;
pub use think::ThinkTool;
pub use traits::{AgentTool, ToolExecutionContext};
pub use web_fetch::WebFetch;

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
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
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
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
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
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
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
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
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

    // ── data-parser: new formats ────────────────────────────────────────────

    #[tokio::test]
    async fn test_data_parser_yaml() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let yaml = "name: Alice\nage: 30\nactive: true";
        let result = tool
            .execute(
                serde_json::json!({"data": yaml, "format": "yaml"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["format"], "yaml");
        assert_eq!(result["parsed"]["name"], "Alice");
        assert_eq!(result["parsed"]["age"], 30);
        assert_eq!(result["parsed"]["active"], true);
    }

    #[tokio::test]
    async fn test_data_parser_tsv() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let tsv = "city\tpop\nLondon\t9000000\nParis\t2100000";
        let result = tool
            .execute(
                serde_json::json!({"data": tsv, "format": "tsv"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["format"], "tsv");
        assert_eq!(result["parsed"]["row_count"], 2);
        assert_eq!(result["parsed"]["rows"][0]["city"], "London");
    }

    #[tokio::test]
    async fn test_data_parser_jsonl() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let jsonl = "{\"id\":1,\"msg\":\"hello\"}\n{\"id\":2,\"msg\":\"world\"}";
        let result = tool
            .execute(
                serde_json::json!({"data": jsonl, "format": "jsonl"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["format"], "jsonl");
        assert_eq!(result["parsed"]["count"], 2);
        assert_eq!(result["parsed"]["records"][0]["msg"], "hello");
        assert_eq!(result["parsed"]["records"][1]["id"], 2);
    }

    #[tokio::test]
    async fn test_data_parser_xml() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let xml = "<root><name>Alice</name><age>30</age></root>";
        let result = tool
            .execute(
                serde_json::json!({"data": xml, "format": "xml"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["format"], "xml");
        assert_eq!(result["parsed"]["tag"], "root");
        assert_eq!(result["parsed"]["children"]["name"]["text"], "Alice");
        assert_eq!(result["parsed"]["children"]["age"]["text"], "30");
    }

    #[tokio::test]
    async fn test_data_parser_markdown() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let md = "---\ntitle: Test\n---\n# Heading 1\n## Heading 2\n[link](https://example.com)";
        let result = tool
            .execute(
                serde_json::json!({"data": md, "format": "markdown"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["format"], "markdown");
        assert_eq!(result["parsed"]["frontmatter"]["title"], "Test");
        assert_eq!(result["parsed"]["headings"][0]["level"], 1);
        assert_eq!(result["parsed"]["headings"][1]["text"], "Heading 2");
        assert_eq!(result["parsed"]["links"][0]["url"], "https://example.com");
    }

    // ── data-parser: type coercion ──────────────────────────────────────────

    #[tokio::test]
    async fn test_data_parser_csv_infer_types() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let csv = "name,age,score,active\nAlice,30,9.5,true\nBob,25,7.2,false";
        let result = tool
            .execute(
                serde_json::json!({"data": csv, "format": "csv", "infer_types": true}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        let row0 = &result["parsed"]["rows"][0];
        assert_eq!(row0["age"], 30);
        assert_eq!(row0["score"], 9.5);
        assert_eq!(row0["active"], true);
    }

    // ── data-parser: path query ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_data_parser_query_nested_key() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let json = r#"{"user":{"name":"Alice","age":30}}"#;
        let result = tool
            .execute(
                serde_json::json!({"data": json, "format": "json", "query": ".user.name"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["parsed"], "Alice");
    }

    #[tokio::test]
    async fn test_data_parser_query_array_index() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let json = r#"{"items":[{"id":1},{"id":2},{"id":3}]}"#;
        let result = tool
            .execute(
                serde_json::json!({"data": json, "format": "json", "query": ".items[1].id"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["parsed"], 2);
    }

    #[tokio::test]
    async fn test_data_parser_query_missing_key_errors() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let json = r#"{"a":1}"#;
        let err = tool
            .execute(
                serde_json::json!({"data": json, "format": "json", "query": ".missing"}),
                make_context(dir.path()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
    }

    // ── data-parser: pagination ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_data_parser_csv_pagination() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let csv = "id\n1\n2\n3\n4\n5";
        let result = tool
            .execute(
                serde_json::json!({"data": csv, "format": "csv", "offset": 2, "limit": 2}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["total"], 5);
        assert_eq!(result["offset"], 2);
        assert_eq!(result["limit"], 2);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["parsed"]["rows"][0]["id"], "3");
        assert_eq!(result["parsed"]["rows"][1]["id"], "4");
    }

    #[tokio::test]
    async fn test_data_parser_jsonl_pagination() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let jsonl = "{\"n\":1}\n{\"n\":2}\n{\"n\":3}";
        let result = tool
            .execute(
                serde_json::json!({"data": jsonl, "format": "jsonl", "offset": 1, "limit": 1}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["total"], 3);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["parsed"]["records"][0]["n"], 2);
    }

    #[tokio::test]
    async fn test_data_parser_no_pagination_metadata_without_params() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let csv = "x\n1\n2\n3";
        let result = tool
            .execute(
                serde_json::json!({"data": csv, "format": "csv"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert!(result.get("total").is_none());
        assert!(result.get("has_more").is_none());
    }

    // ── data-parser: cross-format serialization ─────────────────────────────

    #[tokio::test]
    async fn test_data_parser_output_format_json_to_yaml() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let json = r#"{"name":"Alice","age":30}"#;
        let result = tool
            .execute(
                serde_json::json!({"data": json, "format": "json", "output_format": "yaml"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result["output_format"], "yaml");
        let output = result["output"].as_str().unwrap();
        assert!(output.contains("Alice"));
    }

    #[tokio::test]
    async fn test_data_parser_output_format_csv_roundtrip() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let csv = "name,age\nAlice,30\nBob,25";
        let result = tool
            .execute(
                serde_json::json!({"data": csv, "format": "csv", "output_format": "csv"}),
                make_context(dir.path()),
            )
            .await
            .unwrap();
        let output = result["output"].as_str().unwrap();
        assert!(output.contains("Alice"));
        assert!(output.contains("Bob"));
    }

    #[tokio::test]
    async fn test_data_parser_unsupported_format_errors() {
        let tool = DataParser::new();
        let dir = TempDir::new().unwrap();
        let err = tool
            .execute(
                serde_json::json!({"data": "x", "format": "msgpack"}),
                make_context(dir.path()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
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
        let runner = ToolRunner::new(dir.path()).unwrap();
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

    // ── file-editor tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_file_editor_basic_edit() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "Hello, world!").unwrap();

        let tool = crate::file_editor::FileEditor::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({
                    "path": "hello.txt",
                    "edits": [{"old_text": "world", "new_text": "AgentOS"}]
                }),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        assert_eq!(result["edits_applied"], 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
            "Hello, AgentOS!"
        );
    }

    #[tokio::test]
    async fn test_file_editor_multi_edit() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "foo bar baz").unwrap();

        let tool = crate::file_editor::FileEditor::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({
                    "path": "f.txt",
                    "edits": [
                        {"old_text": "foo", "new_text": "FOO"},
                        {"old_text": "baz", "new_text": "BAZ"}
                    ]
                }),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["edits_applied"], 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "FOO bar BAZ"
        );
    }

    #[tokio::test]
    async fn test_file_editor_old_text_not_found() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();

        let tool = crate::file_editor::FileEditor::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({
                    "path": "f.txt",
                    "edits": [{"old_text": "NOPE", "new_text": "x"}]
                }),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
        // File must be unchanged.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn test_file_editor_old_text_ambiguous() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "cat cat cat").unwrap();

        let tool = crate::file_editor::FileEditor::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({
                    "path": "f.txt",
                    "edits": [{"old_text": "cat", "new_text": "dog"}]
                }),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
        assert!(err.to_string().contains("3"));
    }

    #[tokio::test]
    async fn test_file_editor_empty_edits_rejected() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();

        let tool = crate::file_editor::FileEditor::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"path": "f.txt", "edits": []}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    }

    #[tokio::test]
    async fn test_file_editor_path_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_editor::FileEditor::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({
                    "path": "../../etc/passwd",
                    "edits": [{"old_text": "root", "new_text": "pwned"}]
                }),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            AgentOSError::ToolExecutionFailed { .. } | AgentOSError::PermissionDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_file_editor_write_lock_blocked() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("locked.txt"), "data").unwrap();

        let registry = std::sync::Arc::new(crate::file_lock::FileLockRegistry::new());
        let locked_path = dir.path().canonicalize().unwrap().join("locked.txt");
        registry
            .try_acquire(&locked_path, AgentID::new(), TaskID::new())
            .unwrap();

        let ctx = make_context_with_lock_registry(dir.path(), registry);
        let tool = crate::file_editor::FileEditor::new();

        let err = tool
            .execute(
                serde_json::json!({
                    "path": "locked.txt",
                    "edits": [{"old_text": "data", "new_text": "changed"}]
                }),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::FileLocked { .. }));
    }

    #[tokio::test]
    async fn test_file_editor_size_guard() {
        let dir = TempDir::new().unwrap();
        // Write a file just over 10 MiB (10 * 1024 * 1024 + 1 bytes).
        let big = "x".repeat(10 * 1024 * 1024 + 1);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();

        let tool = crate::file_editor::FileEditor::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({
                    "path": "big.txt",
                    "edits": [{"old_text": "x", "new_text": "y"}]
                }),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
        assert!(err.to_string().contains("too large"));
    }

    // ── file-glob tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_file_glob_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        std::fs::write(dir.path().join("c.rs"), "c").unwrap();

        let tool = crate::file_glob::FileGlob::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(serde_json::json!({"pattern": "*.txt"}), ctx)
            .await
            .unwrap();

        assert_eq!(result["count"], 2);
        let paths: Vec<&str> = result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["path"].as_str().unwrap())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("a.txt")));
        assert!(paths.iter().any(|p| p.ends_with("b.txt")));
    }

    #[tokio::test]
    async fn test_file_glob_recursive() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("top.rs"), "").unwrap();
        std::fs::write(dir.path().join("sub").join("nested.rs"), "").unwrap();

        let tool = crate::file_glob::FileGlob::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(serde_json::json!({"pattern": "**/*.rs"}), ctx)
            .await
            .unwrap();

        assert_eq!(result["count"], 2);
    }

    #[tokio::test]
    async fn test_file_glob_no_matches() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), "").unwrap();

        let tool = crate::file_glob::FileGlob::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(serde_json::json!({"pattern": "*.rs"}), ctx)
            .await
            .unwrap();

        assert_eq!(result["count"], 0);
        assert!(result["matches"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_file_glob_traversal_in_pattern_rejected() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_glob::FileGlob::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"pattern": "../**/*"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn test_file_glob_traversal_in_path_rejected() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_glob::FileGlob::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({"pattern": "*.txt", "path": "../../etc"}),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn test_file_glob_absolute_pattern_rejected() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_glob::FileGlob::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"pattern": "/*.txt"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }

    // ── file-grep tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_file_grep_content_mode() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("code.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let tool = crate::file_grep::FileGrep::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({"pattern": "fn main", "output_mode": "content"}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["output_mode"], "content");
        assert_eq!(result["count"], 1);
        assert_eq!(result["matches"][0]["line"], 1);
        assert!(result["matches"][0]["content"]
            .as_str()
            .unwrap()
            .contains("fn main"));
    }

    #[tokio::test]
    async fn test_file_grep_files_with_matches_mode() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle in a haystack").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing here").unwrap();

        let tool = crate::file_grep::FileGrep::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({"pattern": "needle", "output_mode": "files_with_matches"}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["output_mode"], "files_with_matches");
        assert_eq!(result["count"], 1);
        assert!(result["files"][0].as_str().unwrap().ends_with("a.txt"));
    }

    #[tokio::test]
    async fn test_file_grep_count_mode() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "yes\nyes\nno\nyes").unwrap();

        let tool = crate::file_grep::FileGrep::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({"pattern": "yes", "output_mode": "count"}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["output_mode"], "count");
        assert_eq!(result["match_count"], 3);
    }

    #[tokio::test]
    async fn test_file_grep_case_insensitive() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "Hello World\nhello world").unwrap();

        let tool = crate::file_grep::FileGrep::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({"pattern": "hello", "case_insensitive": true, "output_mode": "count"}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["match_count"], 2);
    }

    #[tokio::test]
    async fn test_file_grep_context_lines() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "before\nTARGET\nafter").unwrap();

        let tool = crate::file_grep::FileGrep::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({"pattern": "TARGET", "context_lines": 1, "output_mode": "content"}),
                ctx,
            )
            .await
            .unwrap();

        let m = &result["matches"][0];
        assert_eq!(m["context_before"][0], "before");
        assert_eq!(m["context_after"][0], "after");
    }

    #[tokio::test]
    async fn test_file_grep_invalid_regex_rejected() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_grep::FileGrep::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"pattern": "[unclosed"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    }

    #[tokio::test]
    async fn test_file_grep_glob_filter() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}").unwrap();
        std::fs::write(dir.path().join("b.txt"), "fn hello() {}").unwrap();

        let tool = crate::file_grep::FileGrep::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(
                serde_json::json!({"pattern": "hello", "glob": "*.rs", "output_mode": "files_with_matches"}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(result["count"], 1);
        assert!(result["files"][0].as_str().unwrap().ends_with(".rs"));
    }

    #[tokio::test]
    async fn test_file_grep_path_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_grep::FileGrep::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), true, false, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({"pattern": "root", "path": "../../etc"}),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            AgentOSError::ToolExecutionFailed { .. } | AgentOSError::PermissionDenied { .. }
        ));
    }

    // ── file-delete tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_file_delete_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("todelete.txt"), "bye").unwrap();

        let tool = crate::file_delete::FileDelete::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(serde_json::json!({"path": "todelete.txt"}), ctx)
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        assert!(!dir.path().join("todelete.txt").exists());
    }

    #[tokio::test]
    async fn test_file_delete_nonexistent_fails() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_delete::FileDelete::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"path": "ghost.txt"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn test_file_delete_directory_rejected() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("mydir")).unwrap();

        let tool = crate::file_delete::FileDelete::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"path": "mydir"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
        assert!(err.to_string().contains("directory"));
    }

    #[tokio::test]
    async fn test_file_delete_path_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_delete::FileDelete::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"path": "../../etc/passwd"}), ctx)
            .await
            .unwrap_err();

        // On Linux /etc/passwd exists so canonicalize succeeds → PermissionDenied;
        // on systems where the path doesn't exist → ToolExecutionFailed.
        assert!(matches!(
            err,
            AgentOSError::ToolExecutionFailed { .. } | AgentOSError::PermissionDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_file_delete_write_lock_blocked() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("locked.txt"), "x").unwrap();

        let registry = std::sync::Arc::new(crate::file_lock::FileLockRegistry::new());
        let locked_path = dir.path().canonicalize().unwrap().join("locked.txt");
        registry
            .try_acquire(&locked_path, AgentID::new(), TaskID::new())
            .unwrap();

        let ctx = make_context_with_lock_registry(dir.path(), registry);
        let tool = crate::file_delete::FileDelete::new();

        let err = tool
            .execute(serde_json::json!({"path": "locked.txt"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::FileLocked { .. }));
    }

    // ── file-move tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_file_move_basic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("old.txt"), "content").unwrap();

        let tool = crate::file_move::FileMove::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let result = tool
            .execute(serde_json::json!({"from": "old.txt", "to": "new.txt"}), ctx)
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        assert!(!dir.path().join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "content"
        );
    }

    #[tokio::test]
    async fn test_file_move_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("src.txt"), "data").unwrap();

        let tool = crate::file_move::FileMove::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        tool.execute(
            serde_json::json!({"from": "src.txt", "to": "subdir/nested/dst.txt"}),
            ctx,
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("subdir/nested/dst.txt")).unwrap(),
            "data"
        );
    }

    #[tokio::test]
    async fn test_file_move_same_path_rejected() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();

        let tool = crate::file_move::FileMove::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"from": "f.txt", "to": "f.txt"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn test_file_move_destination_exists_rejected() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("src.txt"), "source").unwrap();
        std::fs::write(dir.path().join("dst.txt"), "existing").unwrap();

        let tool = crate::file_move::FileMove::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(serde_json::json!({"from": "src.txt", "to": "dst.txt"}), ctx)
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
        assert!(dir.path().join("src.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("dst.txt")).unwrap(),
            "existing"
        );
    }

    #[tokio::test]
    async fn test_file_move_source_not_found() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_move::FileMove::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({"from": "ghost.txt", "to": "other.txt"}),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::ToolExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn test_file_move_source_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        let tool = crate::file_move::FileMove::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        let err = tool
            .execute(
                serde_json::json!({"from": "../../etc/passwd", "to": "pwned.txt"}),
                ctx,
            )
            .await
            .unwrap_err();

        // On Linux /etc/passwd exists → PermissionDenied; otherwise → ToolExecutionFailed.
        assert!(matches!(
            err,
            AgentOSError::ToolExecutionFailed { .. } | AgentOSError::PermissionDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_file_move_write_lock_on_source_blocked() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("locked.txt"), "x").unwrap();

        let registry = std::sync::Arc::new(crate::file_lock::FileLockRegistry::new());
        let locked_path = dir.path().canonicalize().unwrap().join("locked.txt");
        registry
            .try_acquire(&locked_path, AgentID::new(), TaskID::new())
            .unwrap();

        let ctx = make_context_with_lock_registry(dir.path(), registry);
        let tool = crate::file_move::FileMove::new();

        let err = tool
            .execute(
                serde_json::json!({"from": "locked.txt", "to": "dst.txt"}),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, AgentOSError::FileLocked { .. }));
    }

    #[tokio::test]
    async fn test_file_move_destination_traversal_blocked() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("src.txt"), "data").unwrap();

        let tool = crate::file_move::FileMove::new();
        let mut perms = PermissionSet::new();
        perms.grant("fs.user_data".to_string(), false, true, false, None);
        let ctx = make_context_with_permissions(dir.path(), perms);

        // A lexically normalised path like data_dir/../../pwned.txt resolves
        // outside data_dir — the post-create_dir_all canonicalization check
        // (CRITICAL-1 fix) must catch this even when the intermediate dirs exist.
        let err = tool
            .execute(
                serde_json::json!({"from": "src.txt", "to": "../../pwned.txt"}),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(
            matches!(
                err,
                AgentOSError::PermissionDenied { .. } | AgentOSError::ToolExecutionFailed { .. }
            ),
            "expected traversal to be blocked, got: {}",
            err
        );
        // Source file must be untouched.
        assert!(dir.path().join("src.txt").exists());
    }

    // ── agent-manual integration tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_agent_manual_index_section() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "index"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "index");
        assert!(result["sections"].as_array().unwrap().len() >= 8);
        assert!(result["usage"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_agent_manual_tools_section_empty() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "tools"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "tools");
        assert_eq!(result["count"], 0);
        assert_eq!(result["tools"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_agent_manual_tools_section_with_tools() {
        let dir = TempDir::new().unwrap();
        let summaries = vec![
            crate::agent_manual::ToolSummary {
                name: "file-reader".into(),
                description: "Read files".into(),
                version: "1.1.0".into(),
                permissions: vec!["fs.user_data:r".into()],
                input_schema: None,
                trust_tier: "core".into(),
            },
            crate::agent_manual::ToolSummary {
                name: "http-client".into(),
                description: "HTTP requests".into(),
                version: "1.0.0".into(),
                permissions: vec!["network.outbound:x".into()],
                input_schema: None,
                trust_tier: "core".into(),
            },
        ];
        let tool = crate::agent_manual::AgentManualTool::new(summaries);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "tools"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["count"], 2);
        let tools = result["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"file-reader"));
        assert!(names.contains(&"http-client"));
    }

    #[tokio::test]
    async fn test_agent_manual_tool_detail_found() {
        let dir = TempDir::new().unwrap();
        let summaries = vec![crate::agent_manual::ToolSummary {
            name: "file-reader".into(),
            description: "Read files from data directory".into(),
            version: "1.1.0".into(),
            permissions: vec!["fs.user_data:r".into()],
            input_schema: Some(
                serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            ),
            trust_tier: "core".into(),
        }];
        let tool = crate::agent_manual::AgentManualTool::new(summaries);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(
                serde_json::json!({"section": "tool-detail", "name": "file-reader"}),
                ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["section"], "tool-detail");
        assert_eq!(result["name"], "file-reader");
        assert_eq!(result["version"], "1.1.0");
        assert!(result["input_schema"].is_object());
    }

    #[tokio::test]
    async fn test_agent_manual_tool_detail_not_found() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(
                serde_json::json!({"section": "tool-detail", "name": "nonexistent"}),
                ctx,
            )
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AgentOSError::ToolNotFound(_)));
    }

    #[tokio::test]
    async fn test_agent_manual_tool_detail_missing_name() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "tool-detail"}), ctx)
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            AgentOSError::SchemaValidation(_)
        ));
    }

    #[tokio::test]
    async fn test_agent_manual_permissions_section() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "permissions"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "permissions");
        assert!(result["resource_classes"].as_array().unwrap().len() >= 5);
    }

    #[tokio::test]
    async fn test_agent_manual_memory_section() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "memory"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "memory");
        let tiers = result["tiers"].as_array().unwrap();
        assert_eq!(tiers.len(), 3);
        let tier_names: Vec<&str> = tiers.iter().map(|t| t["tier"].as_str().unwrap()).collect();
        assert!(tier_names.contains(&"semantic"));
        assert!(tier_names.contains(&"episodic"));
        assert!(tier_names.contains(&"procedural"));
    }

    #[tokio::test]
    async fn test_agent_manual_events_section() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "events"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "events");
        assert_eq!(result["categories"].as_array().unwrap().len(), 10);
    }

    #[tokio::test]
    async fn test_agent_manual_commands_section() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "commands"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "commands");
        assert!(result["domains"].as_array().unwrap().len() >= 8);
    }

    #[tokio::test]
    async fn test_agent_manual_errors_section() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "errors"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "errors");
        let errors = result["errors"].as_array().unwrap();
        assert!(errors.len() >= 5);
        for err in errors {
            assert!(err["error"].as_str().is_some());
            assert!(err["cause"].as_str().is_some());
            assert!(err["recovery"].as_str().is_some());
        }
    }

    #[tokio::test]
    async fn test_agent_manual_feedback_section() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "feedback"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "feedback");
        assert!(result["format"]["fields"].as_array().unwrap().len() >= 4);
        assert!(result["example"].as_str().unwrap().contains("[FEEDBACK]"));
    }

    #[tokio::test]
    async fn test_agent_manual_invalid_section() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"section": "nonexistent"}), ctx)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_agent_manual_missing_section_field() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        let ctx = make_context(dir.path());
        let result = tool
            .execute(serde_json::json!({"query": "hello"}), ctx)
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            AgentOSError::SchemaValidation(_)
        ));
    }

    #[tokio::test]
    async fn test_agent_manual_requires_no_permissions() {
        let dir = TempDir::new().unwrap();
        let tool = crate::agent_manual::AgentManualTool::new(vec![]);
        // Use an empty permission set — should still work
        let ctx = make_context_with_permissions(dir.path(), PermissionSet::new());
        let result = tool
            .execute(serde_json::json!({"section": "index"}), ctx)
            .await;
        assert!(
            result.is_ok(),
            "agent-manual should work without any permissions"
        );
    }

    #[tokio::test]
    async fn test_agent_manual_registered_with_summaries() {
        let dir = TempDir::new().unwrap();
        let mut runner = ToolRunner::new(dir.path()).unwrap();
        runner.register_agent_manual(vec![crate::agent_manual::ToolSummary {
            name: "test-tool".into(),
            description: "A test".into(),
            version: "0.1.0".into(),
            permissions: vec![],
            input_schema: None,
            trust_tier: "core".into(),
        }]);
        let tools = runner.list_tools();
        assert!(tools.contains(&"agent-manual".to_string()));

        let ctx = make_context(dir.path());
        let result = runner
            .execute("agent-manual", serde_json::json!({"section": "index"}), ctx)
            .await
            .unwrap();
        assert_eq!(result["section"], "index");
    }
}
