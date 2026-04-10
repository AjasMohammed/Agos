/// Persistent store for runtime MCP server attachments.
///
/// Attachments made via `agentos mcp attach` are saved here so they survive
/// kernel restarts. The store lives at `{data_dir}/mcp_attachments.db`.
///
/// Env var values may contain `vault:KEY` references — they are stored as-is
/// and resolved from the vault each time the kernel boots or the server connects.
use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A persisted MCP server attachment record.
#[derive(Clone, Serialize, Deserialize)]
pub struct McpAttachmentRecord {
    pub name: String,
    /// Executable for stdio transport (e.g. `"npx"`). `None` for HTTP.
    pub command: Option<String>,
    /// Arguments for the stdio executable.
    pub args: Vec<String>,
    /// HTTP endpoint URL. `None` for stdio.
    pub url: Option<String>,
    /// Static auth token for HTTP transport. `None` for stdio or OAuth.
    pub auth_token: Option<String>,
    /// OAuth2 connector ID referencing a credential in the vault.
    /// When set, the transport is built with a `VaultOAuthProvider`.
    pub oauth_connector_id: Option<String>,
    /// Environment variables for the subprocess.
    /// Values may be `"vault:KEY"` to be resolved at spawn time.
    pub env: HashMap<String, String>,
    /// Per-request timeout in seconds.
    pub timeout_secs: Option<u64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl std::fmt::Debug for McpAttachmentRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpAttachmentRecord")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("url", &self.url)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("oauth_connector_id", &self.oauth_connector_id)
            .field("env", &"[REDACTED]")
            .field("timeout_secs", &self.timeout_secs)
            .field("created_at", &self.created_at)
            .finish()
    }
}

pub struct McpAttachmentStore {
    conn: Arc<Mutex<Connection>>,
}

impl McpAttachmentStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            let conn = Connection::open(&path)
                .with_context(|| format!("open mcp_attachments.db at {}", path.display()))?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS mcp_attachments (
                    name                TEXT PRIMARY KEY,
                    command             TEXT,
                    args_json           TEXT NOT NULL,
                    url                 TEXT,
                    auth_token          TEXT,
                    oauth_connector_id  TEXT,
                    env_json            TEXT NOT NULL,
                    timeout_secs        INTEGER,
                    created_at          TEXT NOT NULL
                );
                -- Idempotent migration: add oauth_connector_id if this DB was
                -- created before the column existed.
                ALTER TABLE mcp_attachments ADD COLUMN oauth_connector_id TEXT;",
            )
            // Ignore the error from ALTER TABLE if the column already exists.
            .or_else(|e| {
                let msg = e.to_string();
                if msg.contains("duplicate column") {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
            Ok(conn)
        })
        .await
        .context("spawn_blocking for mcp_attachments.db open")??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Upsert a runtime attachment record.
    pub async fn save(&self, record: McpAttachmentRecord) -> anyhow::Result<()> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let args_json = serde_json::to_string(&record.args)?;
            let env_json = serde_json::to_string(&record.env)?;
            conn.execute(
                "INSERT INTO mcp_attachments
                    (name, command, args_json, url, auth_token, oauth_connector_id, env_json, timeout_secs, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(name) DO UPDATE SET
                    command             = excluded.command,
                    args_json           = excluded.args_json,
                    url                 = excluded.url,
                    auth_token          = excluded.auth_token,
                    oauth_connector_id  = excluded.oauth_connector_id,
                    env_json            = excluded.env_json,
                    timeout_secs        = excluded.timeout_secs,
                    created_at          = excluded.created_at",
                params![
                    record.name,
                    record.command,
                    args_json,
                    record.url,
                    record.auth_token,
                    record.oauth_connector_id,
                    env_json,
                    record.timeout_secs.map(|v| v as i64),
                    record.created_at.to_rfc3339(),
                ],
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("spawn_blocking save mcp_attachment")??;
        Ok(())
    }

    /// Delete an attachment record by name.
    pub async fn delete(&self, name: &str) -> anyhow::Result<bool> {
        let conn = Arc::clone(&self.conn);
        let name = name.to_string();
        let rows = tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.execute("DELETE FROM mcp_attachments WHERE name = ?1", params![name])
                .map_err(anyhow::Error::from)
        })
        .await
        .context("spawn_blocking delete mcp_attachment")??;
        Ok(rows > 0)
    }

    /// Load all persisted attachment records.
    pub async fn list_all(&self) -> anyhow::Result<Vec<McpAttachmentRecord>> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT name, command, args_json, url, auth_token, oauth_connector_id, env_json, timeout_secs, created_at
                 FROM mcp_attachments ORDER BY created_at ASC",
            )?;
            let records = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,               // name
                        row.get::<_, Option<String>>(1)?,       // command
                        row.get::<_, String>(2)?,               // args_json
                        row.get::<_, Option<String>>(3)?,       // url
                        row.get::<_, Option<String>>(4)?,       // auth_token
                        row.get::<_, Option<String>>(5)?,       // oauth_connector_id
                        row.get::<_, String>(6)?,               // env_json
                        row.get::<_, Option<i64>>(7)?,          // timeout_secs
                        row.get::<_, String>(8)?,               // created_at
                    ))
                })?
                .filter_map(|r| match r {
                    Ok(row) => Some(row),
                    Err(e) => {
                        tracing::warn!(error = %e, "Skipping corrupt MCP attachment row from SQLite");
                        None
                    }
                })
                .filter_map(|(name, command, args_json, url, auth_token, oauth_connector_id, env_json, timeout_secs, created_at)| {
                    let args: Vec<String> = match serde_json::from_str(&args_json) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(server = %name, error = %e, "Skipping MCP attachment — corrupt args_json");
                            return None;
                        }
                    };
                    let env: HashMap<String, String> = match serde_json::from_str(&env_json) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(server = %name, error = %e, "Skipping MCP attachment — corrupt env_json");
                            return None;
                        }
                    };
                    let created_at = match chrono::DateTime::parse_from_rfc3339(&created_at) {
                        Ok(dt) => dt.with_timezone(&chrono::Utc),
                        Err(e) => {
                            tracing::warn!(server = %name, error = %e, "Skipping MCP attachment — corrupt created_at");
                            return None;
                        }
                    };
                    Some(McpAttachmentRecord {
                        name,
                        command,
                        args,
                        url,
                        auth_token,
                        oauth_connector_id,
                        env,
                        timeout_secs: timeout_secs.map(|v| v as u64),
                        created_at,
                    })
                })
                .collect::<Vec<_>>();
            Ok::<_, anyhow::Error>(records)
        })
        .await
        .context("spawn_blocking list_all mcp_attachments")?
    }

    /// Check whether an attachment with this name exists.
    pub async fn exists(&self, name: &str) -> anyhow::Result<bool> {
        let conn = Arc::clone(&self.conn);
        let name = name.to_string();
        let found = tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            conn.query_row(
                "SELECT 1 FROM mcp_attachments WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()
            .map_err(anyhow::Error::from)
        })
        .await
        .context("spawn_blocking exists mcp_attachment")??;
        Ok(found.is_some())
    }
}
