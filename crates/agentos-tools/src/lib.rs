pub mod data_parser;
pub mod file_reader;
pub mod file_writer;
pub mod loader;
pub mod memory_search;
pub mod memory_write;
pub mod runner;
pub mod shell_exec;
pub mod traits;
pub mod agent_message;
pub mod task_delegate;

pub use data_parser::DataParser;
pub use file_reader::FileReader;
pub use file_writer::FileWriter;
pub use loader::{load_all_manifests, load_manifest};
pub use memory_search::MemorySearch;
pub use memory_write::MemoryWrite;
pub use runner::ToolRunner;
pub use shell_exec::ShellExec;
pub use traits::{AgentTool, ToolExecutionContext};
pub use agent_message::AgentMessageTool;
pub use task_delegate::TaskDelegate;

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;
    use crate::traits::ToolExecutionContext;

    #[tokio::test]
    async fn test_agent_message_tool() {
        let tool = crate::agent_message::AgentMessageTool::new();
        let payload = serde_json::json!({
            "to": "analyst",
            "content": "Analyze the log"
        });

        let ctx = ToolExecutionContext {
            task_id: TaskID::new(),
            data_dir: std::path::PathBuf::from("/tmp"),
            trace_id: TraceID::new(),
        };

        let result = tool.execute(payload, ctx).await.unwrap();
        assert_eq!(result.get("_kernel_action").unwrap().as_str().unwrap(), "send_agent_message");
        assert_eq!(result.get("to").unwrap().as_str().unwrap(), "analyst");
        assert_eq!(result.get("content").unwrap().as_str().unwrap(), "Analyze the log");
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
            data_dir: std::path::PathBuf::from("/tmp"),
            trace_id: TraceID::new(),
        };

        let result = tool.execute(payload, ctx).await.unwrap();
        assert_eq!(result.get("_kernel_action").unwrap().as_str().unwrap(), "delegate_task");
        assert_eq!(result.get("target_agent").unwrap().as_str().unwrap(), "researcher");
        assert_eq!(result.get("task").unwrap().as_str().unwrap(), "Find top 10 error sources");
        assert_eq!(result.get("priority").unwrap().as_u64().unwrap(), 8);
    }
    use std::path::Path;
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

        let search_tool = MemorySearch::new(dir.path());
        let write_tool = MemoryWrite::new(dir.path());

        // Write a memory entry
        let write_result = write_tool
            .execute(
                serde_json::json!({"content": "Q1 revenue was 2.5 million dollars", "source": "analyst", "tags": "revenue,q1"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert_eq!(write_result["success"], true);

        // Search for it
        let search_result = search_tool
            .execute(
                serde_json::json!({"query": "revenue", "limit": 5}),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(search_result["count"], 1);
        assert!(search_result["results"][0]["content"]
            .as_str()
            .unwrap()
            .contains("2.5 million"));
    }

    #[tokio::test]
    async fn test_episodic_memory_write_and_search() {
        let dir = TempDir::new().unwrap();
        let ctx = make_context(dir.path());

        // We need to create the episodic memory DB first like the kernel would do
        let episodic_db_path = dir.path().join("episodic_memory.db");
        let conn = rusqlite::Connection::open(&episodic_db_path).unwrap();
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS episodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                entry_type TEXT NOT NULL,
                content TEXT NOT NULL,
                metadata TEXT,
                timestamp TEXT NOT NULL,
                trace_id TEXT NOT NULL
            );
        ").unwrap();
        drop(conn);

        let search_tool = MemorySearch::new(dir.path());
        let write_tool = MemoryWrite::new(dir.path());

        // Write a memory entry with episodic scope
        let write_result = write_tool
            .execute(
                serde_json::json!({"content": "Episodic event: agent booted up", "scope": "episodic"}),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert_eq!(write_result["success"], true);

        // Search for it with episodic scope
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
    async fn test_tool_runner_lists_all_built_in_tools() {
        let dir = TempDir::new().unwrap();
        let runner = ToolRunner::new(dir.path());
        let tools = runner.list_tools();

        assert!(tools.len() >= 5, "Expected at least 5 built-in tools, got {}", tools.len());
        assert!(tools.contains(&"file-reader".to_string()));
        assert!(tools.contains(&"file-writer".to_string()));
        assert!(tools.contains(&"memory-search".to_string()));
        assert!(tools.contains(&"memory-write".to_string()));
        assert!(tools.contains(&"data-parser".to_string()));
    }
}
