use agentos_types::{AgentID, AgentOSError};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct MemoryBlock {
    pub id: String,
    pub agent_id: AgentID,
    pub label: String,
    pub content: String,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

impl MemoryBlock {
    pub const MAX_SIZE: usize = 2048;
}

pub struct MemoryBlockStore {
    db: Mutex<Connection>,
}

impl MemoryBlockStore {
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError> {
        let db_path = data_dir.join("memory_blocks.db");
        let conn = Connection::open(&db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open memory block DB: {}", e))
        })?;
        Self::init_schema(&conn)?;
        Ok(Self {
            db: Mutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), AgentOSError> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS memory_blocks (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                label TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(agent_id, label)
            );
            CREATE INDEX IF NOT EXISTS idx_memory_blocks_agent ON memory_blocks(agent_id);
            ",
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to init memory blocks: {}", e)))
    }

    pub fn list(&self, agent_id: &AgentID) -> Result<Vec<MemoryBlock>, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory blocks DB".to_string())
        })?;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, label, content, created_at, updated_at
                 FROM memory_blocks
                 WHERE agent_id = ?1
                 ORDER BY label ASC",
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
        let rows = stmt
            .query_map(params![agent_id.as_uuid().to_string()], |row| {
                Ok(Self::row_to_block(row))
            })
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
        let mut blocks = Vec::new();
        for row in rows {
            blocks.push(row.map_err(|e| AgentOSError::StorageError(e.to_string()))?);
        }
        Ok(blocks)
    }

    pub fn get(
        &self,
        agent_id: &AgentID,
        label: &str,
    ) -> Result<Option<MemoryBlock>, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory blocks DB".to_string())
        })?;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, label, content, created_at, updated_at
                 FROM memory_blocks WHERE agent_id = ?1 AND label = ?2",
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
        let result = stmt
            .query_row(params![agent_id.as_uuid().to_string(), label], |row| {
                Ok(Self::row_to_block(row))
            })
            .optional()
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
        Ok(result)
    }

    pub fn write(
        &self,
        agent_id: &AgentID,
        label: &str,
        content: &str,
    ) -> Result<MemoryBlock, AgentOSError> {
        if content.len() > MemoryBlock::MAX_SIZE {
            return Err(AgentOSError::SchemaValidation(format!(
                "Memory block too large: {} > {}",
                content.len(),
                MemoryBlock::MAX_SIZE
            )));
        }
        if label.is_empty() || label.len() > 128 {
            return Err(AgentOSError::SchemaValidation(
                "Memory block label must be 1..128 chars".to_string(),
            ));
        }

        let now = Utc::now().to_rfc3339();
        let agent = agent_id.as_uuid().to_string();
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory blocks DB".to_string())
        })?;
        let existing_id: Option<String> = conn
            .query_row(
                "SELECT id FROM memory_blocks WHERE agent_id = ?1 AND label = ?2",
                params![agent, label],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;

        let block_id = if let Some(existing) = existing_id {
            conn.execute(
                "UPDATE memory_blocks SET content = ?1, updated_at = ?2 WHERE id = ?3",
                params![content, now, existing],
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            existing
        } else {
            let new_id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO memory_blocks (id, agent_id, label, content, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![new_id, agent, label, content, now, now],
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
            new_id
        };
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, label, content, created_at, updated_at
                 FROM memory_blocks WHERE id = ?1",
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
        let block = stmt
            .query_row(params![block_id], |row| Ok(Self::row_to_block(row)))
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
        if &block.agent_id != agent_id {
            return Err(AgentOSError::PermissionDenied {
                resource: "memory.blocks".to_string(),
                operation: "Write".to_string(),
            });
        }
        Ok(block)
    }

    pub fn delete(&self, agent_id: &AgentID, label: &str) -> Result<bool, AgentOSError> {
        let conn = self.db.lock().map_err(|_| {
            AgentOSError::StorageError("Failed to lock memory blocks DB".to_string())
        })?;
        let deleted = conn
            .execute(
                "DELETE FROM memory_blocks WHERE agent_id = ?1 AND label = ?2",
                params![agent_id.as_uuid().to_string(), label],
            )
            .map_err(|e| AgentOSError::StorageError(e.to_string()))?;
        Ok(deleted > 0)
    }

    pub fn blocks_for_context(&self, agent_id: &AgentID) -> Result<String, AgentOSError> {
        let blocks = self.list(agent_id)?;
        if blocks.is_empty() {
            return Ok(String::new());
        }
        Ok(blocks
            .iter()
            .map(|b| format!("[{}]\n{}", b.label, b.content))
            .collect::<Vec<_>>()
            .join("\n\n"))
    }

    fn row_to_block(row: &rusqlite::Row<'_>) -> MemoryBlock {
        let id: String = row.get(0).unwrap_or_default();
        let agent_id_str: String = row.get(1).unwrap_or_default();
        let label: String = row.get(2).unwrap_or_default();
        let content: String = row.get(3).unwrap_or_default();
        let created_at_str: String = row.get(4).unwrap_or_default();
        let updated_at_str: String = row.get(5).unwrap_or_default();

        let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        MemoryBlock {
            id,
            agent_id: AgentID::from_uuid(Uuid::parse_str(&agent_id_str).unwrap_or_default()),
            label,
            content,
            created_at,
            updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_read_list_delete_cycle() {
        let dir = TempDir::new().expect("temp dir");
        let store = MemoryBlockStore::open(dir.path()).expect("store open");
        let agent_id = AgentID::new();

        let saved = store
            .write(&agent_id, "prefs", "Always use concise responses")
            .expect("write");
        assert_eq!(saved.label, "prefs");

        let fetched = store
            .get(&agent_id, "prefs")
            .expect("read")
            .expect("exists");
        assert_eq!(fetched.content, "Always use concise responses");

        let listed = store.list(&agent_id).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "prefs");

        let deleted = store.delete(&agent_id, "prefs").expect("delete");
        assert!(deleted);
        let after = store.list(&agent_id).expect("list");
        assert!(after.is_empty());
    }

    #[test]
    fn oversize_block_is_rejected() {
        let dir = TempDir::new().expect("temp dir");
        let store = MemoryBlockStore::open(dir.path()).expect("store open");
        let agent_id = AgentID::new();
        let too_large = "x".repeat(MemoryBlock::MAX_SIZE + 1);
        let err = store
            .write(&agent_id, "large", &too_large)
            .expect_err("expected oversize rejection");
        assert!(matches!(err, AgentOSError::SchemaValidation(_)));
    }
}
