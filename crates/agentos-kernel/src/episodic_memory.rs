use agentos_types::*;
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EpisodeType {
    Intent,
    ToolCall,
    ToolResult,
    LLMResponse,
    AgentMessage,
    UserPrompt,
}

impl EpisodeType {
    fn as_str(&self) -> &'static str {
        match self {
            EpisodeType::Intent => "intent",
            EpisodeType::ToolCall => "tool_call",
            EpisodeType::ToolResult => "tool_result",
            EpisodeType::LLMResponse => "llm_response",
            EpisodeType::AgentMessage => "agent_message",
            EpisodeType::UserPrompt => "user_prompt",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "intent" => Some(EpisodeType::Intent),
            "tool_call" => Some(EpisodeType::ToolCall),
            "tool_result" => Some(EpisodeType::ToolResult),
            "llm_response" => Some(EpisodeType::LLMResponse),
            "agent_message" => Some(EpisodeType::AgentMessage),
            "user_prompt" => Some(EpisodeType::UserPrompt),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: i64,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub entry_type: EpisodeType,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub trace_id: TraceID,
}

pub struct EpisodicMemory {
    db: Mutex<Connection>,
}

impl EpisodicMemory {
    /// Open or create the episodic memory database.
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError> {
        let db_path = data_dir.join("episodic_memory.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| AgentOSError::StorageError(format!("Failed to open episodic memory DB: {}", e)))?;

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
            CREATE INDEX IF NOT EXISTS idx_episodes_task ON episodes(task_id);
            CREATE INDEX IF NOT EXISTS idx_episodes_agent ON episodes(agent_id);
            CREATE INDEX IF NOT EXISTS idx_episodes_timestamp ON episodes(timestamp);
        ").map_err(|e| AgentOSError::StorageError(format!("Failed to init episodic memory tables: {}", e)))?;

        Ok(Self { db: Mutex::new(conn) })
    }

    /// Record an episode entry.
    pub fn record(
        &self,
        task_id: &TaskID,
        agent_id: &AgentID,
        entry_type: EpisodeType,
        content: &str,
        metadata: Option<serde_json::Value>,
        trace_id: &TraceID,
    ) -> Result<(), AgentOSError> {
        let conn = self.db.lock().unwrap();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let metadata_str = metadata.map(|v| v.to_string());

        conn.execute(
            "INSERT INTO episodes (task_id, agent_id, entry_type, content, metadata, timestamp, trace_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
             params![
                 task_id.as_uuid().to_string(),
                 agent_id.as_uuid().to_string(),
                 entry_type.as_str(),
                 content,
                 metadata_str,
                 timestamp,
                 trace_id.as_uuid().to_string()
             ]
        ).map_err(|e| AgentOSError::StorageError(format!("Failed to record episode: {}", e)))?;

        Ok(())
    }

    /// Query episodes for a specific task.
    pub fn query_by_task(
        &self,
        task_id: &TaskID,
        limit: u32,
    ) -> Result<Vec<Episode>, AgentOSError> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, agent_id, entry_type, content, metadata, timestamp, trace_id
             FROM episodes WHERE task_id = ?1 ORDER BY timestamp DESC LIMIT ?2"
        ).map_err(|e| AgentOSError::StorageError(format!("Failed to prepare query: {}", e)))?;

        let task_id_str = task_id.as_uuid().to_string();
        let episode_iter = stmt.query_map(params![task_id_str, limit], |row| Self::row_to_episode(row))
            .map_err(|e| AgentOSError::StorageError(format!("Failed to query task episodes: {}", e)))?;

        let mut episodes = Vec::new();
        for r in episode_iter {
            if let Ok(ep) = r {
                episodes.push(ep);
            }
        }

        // Reverse because we queried DESC and we want chronological order usually
        episodes.reverse();
        Ok(episodes)
    }

    /// Query all episodes for an agent across tasks.
    pub fn query_by_agent(
        &self,
        agent_id: &AgentID,
        limit: u32,
    ) -> Result<Vec<Episode>, AgentOSError> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, agent_id, entry_type, content, metadata, timestamp, trace_id
             FROM episodes WHERE agent_id = ?1 ORDER BY timestamp DESC LIMIT ?2"
        ).map_err(|e| AgentOSError::StorageError(format!("Failed to prepare query: {}", e)))?;

        let agent_id_str = agent_id.as_uuid().to_string();
        let episode_iter = stmt.query_map(params![agent_id_str, limit], |row| Self::row_to_episode(row))
            .map_err(|e| AgentOSError::StorageError(format!("Failed to query agent episodes: {}", e)))?;

        let mut episodes = Vec::new();
        for r in episode_iter {
            if let Ok(ep) = r {
                episodes.push(ep);
            }
        }

        episodes.reverse();
        Ok(episodes)
    }

    /// Helper to convert a sqlite row to an Episode.
    fn row_to_episode(row: &rusqlite::Row) -> SqliteResult<Episode> {
        let task_id_str: String = row.get(1)?;
        let agent_id_str: String = row.get(2)?;
        let entry_type_str: String = row.get(3)?;
        let trace_id_str: String = row.get(7)?;

        let metadata_str: Option<String> = row.get(5)?;
        let metadata = metadata_str.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

        let timestamp_str: String = row.get(6)?;
        let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_str).unwrap().with_timezone(&chrono::Utc);

        Ok(Episode {
            id: row.get(0)?,
            task_id: TaskID::from_uuid(uuid::Uuid::parse_str(&task_id_str).unwrap_or_default()),
            agent_id: AgentID::from_uuid(uuid::Uuid::parse_str(&agent_id_str).unwrap_or_default()),
            entry_type: EpisodeType::from_str(&entry_type_str).unwrap_or(EpisodeType::Intent),
            content: row.get(4)?,
            metadata,
            timestamp,
            trace_id: TraceID::from_uuid(uuid::Uuid::parse_str(&trace_id_str).unwrap_or_default()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_episodic_memory_record_and_query() {
        let dir = TempDir::new().unwrap();
        let mem = EpisodicMemory::open(dir.path()).unwrap();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();
        let trace_id = TraceID::new();

        mem.record(&task_id, &agent_id, EpisodeType::UserPrompt, "Hello", None, &trace_id).unwrap();
        mem.record(&task_id, &agent_id, EpisodeType::LLMResponse, "Hi there", None, &trace_id).unwrap();

        let episodes = mem.query_by_task(&task_id, 10).unwrap();
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0].content, "Hello");
        assert_eq!(episodes[1].content, "Hi there");
    }
}
