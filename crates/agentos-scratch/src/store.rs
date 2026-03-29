use crate::error::ScratchError;
use crate::links::{parse_wikilinks, WikiLink};
use crate::types::{LinkInfo, PageSummary, ScratchPage, SearchResult};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;

/// Convert a mutex poison error or JoinError into a ScratchError.
fn internal_err(msg: impl std::fmt::Display) -> ScratchError {
    ScratchError::Internal(msg.to_string())
}

/// Maximum content size per page (64 KB).
const MAX_CONTENT_BYTES: usize = 64 * 1024;

/// Maximum number of pages per agent.
const MAX_PAGES_PER_AGENT: usize = 1000;

/// Maximum title length in characters.
const MAX_TITLE_CHARS: usize = 256;

/// Over-fetch multiplier for tag-filtered searches to compensate for post-query filtering.
const SEARCH_OVERFETCH_MULTIPLIER: usize = 5;

/// SQLite-backed storage engine for agent scratchpad pages with FTS5 search.
pub struct ScratchpadStore {
    conn: Arc<Mutex<Connection>>,
}

impl ScratchpadStore {
    /// Open or create a scratchpad database at the given path.
    pub fn new(db_path: &Path) -> Result<Self, ScratchError> {
        let conn = Connection::open(db_path)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Create an in-memory scratchpad store (for testing).
    #[cfg(test)]
    pub fn in_memory() -> Result<Self, ScratchError> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), ScratchError> {
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        // Uses INTEGER PRIMARY KEY AUTOINCREMENT for stable rowid alignment with FTS5
        // content-sync triggers. The UUID `id` is a separate UNIQUE column.
        // This matches the convention in agentos-memory/src/episodic.rs.
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS pages (
                rowid INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT UNIQUE NOT NULL,
                agent_id TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                metadata TEXT,
                tags TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(agent_id, title)
            );

            CREATE INDEX IF NOT EXISTS idx_pages_agent ON pages(agent_id);

            CREATE TABLE IF NOT EXISTS link_index (
                source_id TEXT NOT NULL,
                target_title TEXT NOT NULL COLLATE NOCASE,
                agent_id TEXT NOT NULL,
                link_text TEXT NOT NULL,
                is_cross_agent INTEGER NOT NULL DEFAULT 0,
                target_agent_id TEXT NOT NULL DEFAULT '',
                UNIQUE(source_id, target_title, agent_id, target_agent_id)
            );

            CREATE INDEX IF NOT EXISTS idx_links_target ON link_index(agent_id, target_title);
            CREATE INDEX IF NOT EXISTS idx_links_source ON link_index(agent_id, source_id);

            CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
                title, content, tags,
                content='pages',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS pages_ai AFTER INSERT ON pages BEGIN
                INSERT INTO pages_fts(rowid, title, content, tags)
                VALUES (new.rowid, new.title, new.content, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS pages_ad AFTER DELETE ON pages BEGIN
                INSERT INTO pages_fts(pages_fts, rowid, title, content, tags)
                VALUES ('delete', old.rowid, old.title, old.content, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS pages_au AFTER UPDATE ON pages BEGIN
                INSERT INTO pages_fts(pages_fts, rowid, title, content, tags)
                VALUES ('delete', old.rowid, old.title, old.content, old.tags);
                INSERT INTO pages_fts(rowid, title, content, tags)
                VALUES (new.rowid, new.title, new.content, new.tags);
            END;
            ",
        )?;
        Ok(())
    }

    /// Validate a title for length, emptiness, and control characters.
    fn validate_title(title: &str) -> Result<(), ScratchError> {
        if title.is_empty() {
            return Err(ScratchError::EmptyTitle);
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(ScratchError::TitleTooLong {
                length: title.chars().count(),
                max: MAX_TITLE_CHARS,
            });
        }
        if title.chars().any(|c| c.is_control()) {
            return Err(ScratchError::InvalidTitle);
        }
        Ok(())
    }

    /// Parse tags JSON defensively for list-style operations.
    fn parse_tags_lossy(tags_str: Option<String>) -> Vec<String> {
        tags_str
            .map(|s| serde_json::from_str(&s).unwrap_or_default())
            .unwrap_or_default()
    }

    /// Render a canonical wiki-link representation for storage/debugging.
    fn format_link_text(link: &WikiLink) -> String {
        let target = if let Some(agent_id) = &link.agent_id {
            format!("@{agent_id}/{}", link.target)
        } else {
            link.target.clone()
        };

        match &link.display {
            Some(display) => format!("[[{target}|{display}]]"),
            None => format!("[[{target}]]"),
        }
    }

    /// Write or upsert a page. If a page with the same (agent_id, title) exists,
    /// its content and tags are updated.
    pub async fn write_page(
        &self,
        agent_id: &str,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<ScratchPage, ScratchError> {
        // Validation
        Self::validate_title(title)?;
        if content.len() > MAX_CONTENT_BYTES {
            return Err(ScratchError::ContentTooLarge {
                size: content.len(),
                max: MAX_CONTENT_BYTES,
            });
        }

        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();
        let title = title.to_string();
        let content = content.to_string();
        let tags = tags.to_vec();

        tokio::task::spawn_blocking(move || {
            let mut db = conn.lock().map_err(internal_err)?;
            let now = Utc::now();
            let tags_json = serde_json::to_string(&tags)?;
            let links = parse_wikilinks(&content);
            let tx = db.transaction()?;

            // Check if this is an update or insert.
            let existing_id: Option<String> = tx
                .query_row(
                    "SELECT id FROM pages WHERE agent_id = ?1 AND title = ?2",
                    params![&agent_id, &title],
                    |row| row.get(0),
                )
                .optional()?;

            let (page_id, updated_existing) = if let Some(id) = existing_id {
                tx.execute(
                    "UPDATE pages SET content = ?1, tags = ?2, updated_at = ?3 WHERE id = ?4",
                    params![&content, &tags_json, now.to_rfc3339(), &id],
                )?;
                (id, true)
            } else {
                // Atomic insert with page count check — prevents TOCTOU race.
                // The INSERT only succeeds if the agent has fewer than MAX_PAGES_PER_AGENT pages.
                let id = Uuid::new_v4().to_string();
                let created_at = now.to_rfc3339();

                let inserted = tx.execute(
                    "INSERT INTO pages (id, agent_id, title, content, metadata, tags, created_at, updated_at)
                     SELECT ?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7
                     WHERE (SELECT COUNT(*) FROM pages WHERE agent_id = ?2) < ?8",
                    params![
                        &id,
                        &agent_id,
                        &title,
                        &content,
                        &tags_json,
                        created_at,
                        created_at,
                        MAX_PAGES_PER_AGENT as i64
                    ],
                )?;

                if inserted == 0 {
                    let count: usize = tx.query_row(
                        "SELECT COUNT(*) FROM pages WHERE agent_id = ?1",
                        params![&agent_id],
                        |row| row.get(0),
                    )?;
                    return Err(ScratchError::TooManyPages {
                        agent_id: agent_id.clone(),
                        count,
                        max: MAX_PAGES_PER_AGENT,
                    });
                }
                (id, false)
            };

            // Replace outbound link index rows atomically with the page write.
            tx.execute(
                "DELETE FROM link_index WHERE source_id = ?1 AND agent_id = ?2",
                params![&page_id, &agent_id],
            )?;

            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO link_index
                 (source_id, target_title, agent_id, link_text, is_cross_agent, target_agent_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;

            for link in &links {
                let target_agent = link.agent_id.as_deref().unwrap_or("");
                stmt.execute(params![
                    &page_id,
                    &link.target,
                    &agent_id,
                    Self::format_link_text(link),
                    if link.is_cross_agent { 1_i64 } else { 0_i64 },
                    target_agent,
                ])?;
            }

            drop(stmt);
            tx.commit()?;

            if updated_existing {
                debug!(agent_id = %agent_id, title = %title, "Updated scratchpad page");
            } else {
                debug!(agent_id = %agent_id, title = %title, "Created scratchpad page");
            }

            Self::read_page_sync(&db, &agent_id, &title)
        })
        .await
        .map_err(internal_err)?
    }

    /// Read a page by agent_id and title.
    pub async fn read_page(
        &self,
        agent_id: &str,
        title: &str,
    ) -> Result<ScratchPage, ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();
        let title = title.to_string();

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;
            Self::read_page_sync(&db, &agent_id, &title)
        })
        .await
        .map_err(internal_err)?
    }

    /// Parse an RFC3339 timestamp from the database, logging a warning on failure.
    fn parse_timestamp(raw: &str, field: &str) -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|e| {
                warn!(raw = %raw, field = %field, error = %e, "Corrupt timestamp in scratchpad, falling back to now");
                Utc::now()
            })
    }

    /// Resolve page UUID from (agent_id, title), returning PageNotFound if missing.
    fn resolve_page_id_sync(
        db: &Connection,
        agent_id: &str,
        title: &str,
    ) -> Result<String, ScratchError> {
        db.query_row(
            "SELECT id FROM pages WHERE agent_id = ?1 AND title = ?2",
            params![agent_id, title],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| ScratchError::PageNotFound {
            agent_id: agent_id.to_string(),
            title: title.to_string(),
        })
    }

    /// Synchronous page read (used internally within spawn_blocking closures).
    fn read_page_sync(
        db: &Connection,
        agent_id: &str,
        title: &str,
    ) -> Result<ScratchPage, ScratchError> {
        let row = db
            .query_row(
                "SELECT id, agent_id, title, content, metadata, tags, created_at, updated_at
                 FROM pages WHERE agent_id = ?1 AND title = ?2",
                params![agent_id, title],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?;

        match row {
            Some((
                id,
                agent_id,
                title,
                content,
                metadata_str,
                tags_str,
                created_at,
                updated_at,
            )) => {
                let metadata = metadata_str.map(|s| serde_json::from_str(&s)).transpose()?;
                let tags: Vec<String> = tags_str
                    .map(|s| serde_json::from_str(&s))
                    .transpose()?
                    .unwrap_or_default();
                let created_at = Self::parse_timestamp(&created_at, "created_at");
                let updated_at = Self::parse_timestamp(&updated_at, "updated_at");

                Ok(ScratchPage {
                    id,
                    agent_id,
                    title,
                    content,
                    metadata,
                    tags,
                    created_at,
                    updated_at,
                })
            }
            None => Err(ScratchError::PageNotFound {
                agent_id: agent_id.to_string(),
                title: title.to_string(),
            }),
        }
    }

    /// Delete a page by agent_id and title.
    pub async fn delete_page(&self, agent_id: &str, title: &str) -> Result<(), ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();
        let title = title.to_string();

        tokio::task::spawn_blocking(move || {
            let mut db = conn.lock().map_err(internal_err)?;
            let tx = db.transaction()?;
            let page_id = Self::resolve_page_id_sync(&tx, &agent_id, &title)?;

            tx.execute(
                "DELETE FROM pages WHERE id = ?1 AND agent_id = ?2",
                params![&page_id, &agent_id],
            )?;
            tx.execute(
                "DELETE FROM link_index WHERE source_id = ?1 AND agent_id = ?2",
                params![&page_id, &agent_id],
            )?;
            tx.commit()?;

            debug!(agent_id = %agent_id, title = %title, "Deleted scratchpad page");
            Ok(())
        })
        .await
        .map_err(internal_err)?
    }

    /// Shared synchronous helper: query pages that link TO the given page title.
    fn query_backlinks_sync(
        db: &Connection,
        agent_id: &str,
        title: &str,
    ) -> Result<Vec<PageSummary>, ScratchError> {
        let mut stmt = db.prepare(
            "SELECT DISTINCT p.id, p.title, p.tags, p.updated_at
             FROM link_index li
             JOIN pages p ON p.id = li.source_id AND p.agent_id = li.agent_id
             WHERE li.agent_id = ?1 AND li.target_title = ?2 AND li.is_cross_agent = 0
             ORDER BY p.updated_at DESC",
        )?;

        let rows = stmt.query_map(params![agent_id, title], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;

        let mut results = Vec::new();
        for row in rows {
            let (id, page_title, tags_str, updated_at) = row?;
            results.push(PageSummary {
                id,
                title: page_title,
                tags: Self::parse_tags_lossy(tags_str),
                updated_at: Self::parse_timestamp(&updated_at, "updated_at"),
            });
        }
        Ok(results)
    }

    /// Shared synchronous helper: query page titles that `page_id` links TO.
    fn query_outlinks_sync(
        db: &Connection,
        agent_id: &str,
        page_id: &str,
    ) -> Result<Vec<String>, ScratchError> {
        let mut stmt = db.prepare(
            "SELECT target_title
             FROM link_index
             WHERE agent_id = ?1 AND source_id = ?2
             ORDER BY target_title COLLATE NOCASE ASC",
        )?;

        let rows = stmt.query_map(params![agent_id, page_id], |row| row.get::<_, String>(0))?;
        let mut outlinks = Vec::new();
        for row in rows {
            outlinks.push(row?);
        }
        Ok(outlinks)
    }

    /// Get pages that link TO the given page title.
    pub async fn get_backlinks(
        &self,
        agent_id: &str,
        title: &str,
    ) -> Result<Vec<PageSummary>, ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();
        let title = title.to_string();

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;
            Self::query_backlinks_sync(&db, &agent_id, &title)
        })
        .await
        .map_err(internal_err)?
    }

    /// Get page titles that the given page links TO.
    pub async fn get_outlinks(
        &self,
        agent_id: &str,
        title: &str,
    ) -> Result<Vec<String>, ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();
        let title = title.to_string();

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;
            let page_id = Self::resolve_page_id_sync(&db, &agent_id, &title)?;
            Self::query_outlinks_sync(&db, &agent_id, &page_id)
        })
        .await
        .map_err(internal_err)?
    }

    /// Get detailed outlink info including cross-agent metadata.
    ///
    /// Unlike `get_outlinks()`, this returns `OutlinkInfo` structs that include
    /// `is_cross_agent` and `target_agent_id` fields needed for cross-agent traversal.
    pub async fn get_outlinks_detailed(
        &self,
        agent_id: &str,
        title: &str,
    ) -> Result<Vec<crate::types::OutlinkInfo>, ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();
        let title = title.to_string();

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;
            let page_id = Self::resolve_page_id_sync(&db, &agent_id, &title)?;

            let mut stmt = db.prepare(
                "SELECT target_title, is_cross_agent, target_agent_id
                 FROM link_index
                 WHERE agent_id = ?1 AND source_id = ?2
                 ORDER BY target_title COLLATE NOCASE ASC",
            )?;

            let rows = stmt.query_map(params![&agent_id, &page_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;

            let mut results = Vec::new();
            for row in rows {
                let (target_title, is_cross, target_agent): (String, i64, Option<String>) = row?;
                // Convert empty string to None (column is NOT NULL DEFAULT '')
                let target_agent_id = target_agent.filter(|s| !s.is_empty());
                results.push(crate::types::OutlinkInfo {
                    target_title,
                    is_cross_agent: is_cross != 0,
                    target_agent_id,
                });
            }
            Ok(results)
        })
        .await
        .map_err(internal_err)?
    }

    /// Get pages with no inbound links (potential orphans).
    pub async fn get_orphans(&self, agent_id: &str) -> Result<Vec<PageSummary>, ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;
            let mut stmt = db.prepare(
                "SELECT p.id, p.title, p.tags, p.updated_at
                 FROM pages p
                 LEFT JOIN link_index li
                   ON li.agent_id = p.agent_id
                  AND li.target_title = p.title
                  AND li.is_cross_agent = 0
                 WHERE p.agent_id = ?1
                   AND li.source_id IS NULL
                 ORDER BY p.updated_at DESC",
            )?;

            let rows = stmt.query_map(params![&agent_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;

            let mut results = Vec::new();
            for row in rows {
                let (id, title, tags_str, updated_at) = row?;
                results.push(PageSummary {
                    id,
                    title,
                    tags: Self::parse_tags_lossy(tags_str),
                    updated_at: Self::parse_timestamp(&updated_at, "updated_at"),
                });
            }

            Ok(results)
        })
        .await
        .map_err(internal_err)?
    }

    /// Get all link information for a page title.
    pub async fn get_all_links(
        &self,
        agent_id: &str,
        title: &str,
    ) -> Result<LinkInfo, ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();
        let title = title.to_string();

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;
            let page_id = Self::resolve_page_id_sync(&db, &agent_id, &title)?;

            let backlinks = Self::query_backlinks_sync(&db, &agent_id, &title)?;
            let outlinks = Self::query_outlinks_sync(&db, &agent_id, &page_id)?;

            let unresolved = {
                let mut stmt = db.prepare(
                    "SELECT li.target_title
                     FROM link_index li
                     LEFT JOIN pages p
                       ON p.agent_id = li.agent_id
                      AND p.title = li.target_title
                     WHERE li.agent_id = ?1
                       AND li.source_id = ?2
                       AND li.is_cross_agent = 0
                       AND p.id IS NULL
                     ORDER BY li.target_title COLLATE NOCASE ASC",
                )?;
                let rows =
                    stmt.query_map(params![&agent_id, &page_id], |row| row.get::<_, String>(0))?;
                let mut items = Vec::new();
                for row in rows {
                    items.push(row?);
                }
                items
            };

            Ok(LinkInfo {
                backlinks,
                outlinks,
                unresolved,
            })
        })
        .await
        .map_err(internal_err)?
    }

    /// List all pages for an agent (lightweight summaries, ordered by most recently updated).
    pub async fn list_pages(&self, agent_id: &str) -> Result<Vec<PageSummary>, ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;

            let mut stmt = db.prepare(
                "SELECT id, title, tags, updated_at FROM pages
                 WHERE agent_id = ?1 ORDER BY updated_at DESC",
            )?;

            let rows = stmt.query_map(params![agent_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;

            let mut results = Vec::new();
            for row in rows {
                let (id, title, tags_str, updated_at) = row?;
                let updated_at = Self::parse_timestamp(&updated_at, "updated_at");

                results.push(PageSummary {
                    id,
                    title,
                    tags: Self::parse_tags_lossy(tags_str),
                    updated_at,
                });
            }

            Ok(results)
        })
        .await
        .map_err(internal_err)?
    }

    /// Full-text search across an agent's scratchpad pages.
    ///
    /// Searches title, content, and tags. Optionally filters by tags.
    /// Results are ranked by FTS5 relevance (BM25).
    ///
    /// When `tags` is non-empty, filtering is applied in application code after FTS5
    /// matching. The query over-fetches by [`SEARCH_OVERFETCH_MULTIPLIER`] to compensate.
    pub async fn search(
        &self,
        agent_id: &str,
        query: &str,
        tags: &[String],
        limit: usize,
    ) -> Result<Vec<SearchResult>, ScratchError> {
        if query.is_empty() {
            return Err(ScratchError::EmptyQuery);
        }

        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();
        let query = query.to_string();
        let tags = tags.to_vec();
        let limit = limit.min(100);
        // Over-fetch when tag filtering is active to compensate for post-query filtering
        let fetch_limit = if tags.is_empty() {
            limit
        } else {
            (limit * SEARCH_OVERFETCH_MULTIPLIER).min(500)
        };

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;

            let mut stmt = db.prepare(
                "SELECT p.id, p.agent_id, p.title, p.content, p.metadata, p.tags,
                        p.created_at, p.updated_at,
                        snippet(pages_fts, 1, '<b>', '</b>', '...', 32) AS snip,
                        rank
                 FROM pages_fts
                 JOIN pages p ON p.rowid = pages_fts.rowid
                 WHERE pages_fts MATCH ?1 AND p.agent_id = ?2
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            // Escape the query as an FTS5 phrase to prevent syntax errors from
            // unbalanced quotes, parentheses, or other FTS5 operators in user input.
            let fts5_query = format!("\"{}\"", query.replace('"', "\"\""));

            let rows =
                stmt.query_map(params![fts5_query, agent_id, fetch_limit as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, f64>(9)?,
                    ))
                })?;

            let mut results = Vec::new();
            for row in rows {
                if results.len() >= limit {
                    break;
                }

                let (
                    id,
                    agent_id,
                    title,
                    content,
                    metadata_str,
                    tags_str,
                    created_at,
                    updated_at,
                    snippet,
                    rank,
                ) = row?;

                // Defensive: tolerate corrupt JSON in search results rather than failing the entire query
                let metadata = metadata_str
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .unwrap_or(None);
                let page_tags: Vec<String> = tags_str
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .unwrap_or(None)
                    .unwrap_or_default();

                // Apply tag filter if specified
                if !tags.is_empty() && !tags.iter().any(|t| page_tags.contains(t)) {
                    continue;
                }

                let created_at = Self::parse_timestamp(&created_at, "created_at");
                let updated_at = Self::parse_timestamp(&updated_at, "updated_at");

                results.push(SearchResult {
                    page: ScratchPage {
                        id,
                        agent_id,
                        title,
                        content,
                        metadata,
                        tags: page_tags,
                        created_at,
                        updated_at,
                    },
                    snippet,
                    rank,
                });
            }

            Ok(results)
        })
        .await
        .map_err(internal_err)?
    }

    /// Count the number of pages for an agent.
    pub async fn page_count(&self, agent_id: &str) -> Result<usize, ScratchError> {
        let conn = self.conn.clone();
        let agent_id = agent_id.to_string();

        tokio::task::spawn_blocking(move || {
            let db = conn.lock().map_err(internal_err)?;

            let count: usize = db.query_row(
                "SELECT COUNT(*) FROM pages WHERE agent_id = ?1",
                params![agent_id],
                |row| row.get(0),
            )?;

            Ok(count)
        })
        .await
        .map_err(internal_err)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> ScratchpadStore {
        ScratchpadStore::in_memory().expect("Failed to create in-memory store")
    }

    #[tokio::test]
    async fn test_write_and_read() {
        let store = make_store();
        let tags = vec!["rust".to_string(), "notes".to_string()];

        let page = store
            .write_page("agent-1", "My Page", "Hello world", &tags)
            .await
            .unwrap();

        assert_eq!(page.agent_id, "agent-1");
        assert_eq!(page.title, "My Page");
        assert_eq!(page.content, "Hello world");
        assert_eq!(page.tags, tags);
        assert!(!page.id.is_empty());

        // Read it back
        let read = store.read_page("agent-1", "My Page").await.unwrap();
        assert_eq!(read.id, page.id);
        assert_eq!(read.content, "Hello world");
        assert_eq!(read.tags, tags);
    }

    #[tokio::test]
    async fn test_upsert() {
        let store = make_store();
        let tags = vec!["v1".to_string()];

        let page1 = store
            .write_page("agent-1", "Upsert Page", "version 1", &tags)
            .await
            .unwrap();

        let tags2 = vec!["v2".to_string()];
        let page2 = store
            .write_page("agent-1", "Upsert Page", "version 2", &tags2)
            .await
            .unwrap();

        // Same ID (upsert, not duplicate)
        assert_eq!(page1.id, page2.id);
        assert_eq!(page2.content, "version 2");
        assert_eq!(page2.tags, tags2);
        // updated_at should be >= created_at
        assert!(page2.updated_at >= page1.created_at);
    }

    #[tokio::test]
    async fn test_content_too_large() {
        let store = make_store();
        let big_content = "x".repeat(MAX_CONTENT_BYTES + 1);

        let result = store
            .write_page("agent-1", "Big Page", &big_content, &[])
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ScratchError::ContentTooLarge { size, max } => {
                assert_eq!(size, MAX_CONTENT_BYTES + 1);
                assert_eq!(max, MAX_CONTENT_BYTES);
            }
            other => panic!("Expected ContentTooLarge, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_title_too_long() {
        let store = make_store();
        let long_title = "a".repeat(MAX_TITLE_CHARS + 1);

        let result = store
            .write_page("agent-1", &long_title, "content", &[])
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ScratchError::TitleTooLong { .. }
        ));
    }

    #[tokio::test]
    async fn test_empty_title() {
        let store = make_store();
        let result = store.write_page("agent-1", "", "content", &[]).await;
        assert!(matches!(result.unwrap_err(), ScratchError::EmptyTitle));
    }

    #[tokio::test]
    async fn test_invalid_title_control_chars() {
        let store = make_store();
        let result = store
            .write_page("agent-1", "bad\x00title", "content", &[])
            .await;
        assert!(matches!(result.unwrap_err(), ScratchError::InvalidTitle));

        let result2 = store
            .write_page("agent-1", "has\nnewline", "content", &[])
            .await;
        assert!(matches!(result2.unwrap_err(), ScratchError::InvalidTitle));
    }

    #[tokio::test]
    async fn test_page_limit() {
        let store = make_store();

        // Write MAX_PAGES_PER_AGENT pages
        for i in 0..MAX_PAGES_PER_AGENT {
            store
                .write_page("agent-1", &format!("Page {i}"), "content", &[])
                .await
                .unwrap();
        }

        // The next one should fail
        let result = store
            .write_page("agent-1", "One Too Many", "content", &[])
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ScratchError::TooManyPages {
                agent_id,
                count,
                max,
            } => {
                assert_eq!(agent_id, "agent-1");
                assert_eq!(count, MAX_PAGES_PER_AGENT);
                assert_eq!(max, MAX_PAGES_PER_AGENT);
            }
            other => panic!("Expected TooManyPages, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fts_search() {
        let store = make_store();

        store
            .write_page(
                "agent-1",
                "Rust Guide",
                "Rust is a systems programming language focused on safety",
                &["rust".to_string()],
            )
            .await
            .unwrap();

        store
            .write_page(
                "agent-1",
                "Python Guide",
                "Python is a dynamic scripting language",
                &["python".to_string()],
            )
            .await
            .unwrap();

        let results = store.search("agent-1", "safety", &[], 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.title, "Rust Guide");
    }

    #[tokio::test]
    async fn test_tag_filter() {
        let store = make_store();

        store
            .write_page(
                "agent-1",
                "Tagged A",
                "Some content about topic A",
                &["alpha".to_string()],
            )
            .await
            .unwrap();

        store
            .write_page(
                "agent-1",
                "Tagged B",
                "Some content about topic B",
                &["beta".to_string()],
            )
            .await
            .unwrap();

        // Search for "content" but filter to only "alpha" tag
        let results = store
            .search("agent-1", "content", &["alpha".to_string()], 10)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.title, "Tagged A");
    }

    #[tokio::test]
    async fn test_search_empty_query() {
        let store = make_store();
        store
            .write_page("agent-1", "Test", "some content", &[])
            .await
            .unwrap();

        let result = store.search("agent-1", "", &[], 10).await;
        assert!(matches!(result.unwrap_err(), ScratchError::EmptyQuery));
    }

    #[tokio::test]
    async fn test_delete() {
        let store = make_store();

        store
            .write_page("agent-1", "Doomed", "temporary content", &[])
            .await
            .unwrap();

        store.delete_page("agent-1", "Doomed").await.unwrap();

        let result = store.read_page("agent-1", "Doomed").await;
        assert!(matches!(
            result.unwrap_err(),
            ScratchError::PageNotFound { .. }
        ));
    }

    #[tokio::test]
    async fn test_delete_nonexistent() {
        let store = make_store();
        let result = store.delete_page("agent-1", "NoSuchPage").await;
        assert!(matches!(
            result.unwrap_err(),
            ScratchError::PageNotFound { .. }
        ));
    }

    #[tokio::test]
    async fn test_list_pages() {
        let store = make_store();

        store
            .write_page("agent-1", "Page A", "aaa", &["tag1".to_string()])
            .await
            .unwrap();
        store
            .write_page("agent-1", "Page B", "bbb", &[])
            .await
            .unwrap();

        let list = store.list_pages("agent-1").await.unwrap();
        assert_eq!(list.len(), 2);

        let titles: Vec<&str> = list.iter().map(|p| p.title.as_str()).collect();
        assert!(titles.contains(&"Page A"));
        assert!(titles.contains(&"Page B"));
    }

    #[tokio::test]
    async fn test_agent_isolation() {
        let store = make_store();

        store
            .write_page("agent-1", "Secret", "agent 1 only", &[])
            .await
            .unwrap();
        store
            .write_page("agent-2", "Other", "agent 2 only", &[])
            .await
            .unwrap();

        // Agent 2 cannot see Agent 1's pages
        let result = store.read_page("agent-2", "Secret").await;
        assert!(matches!(
            result.unwrap_err(),
            ScratchError::PageNotFound { .. }
        ));

        // List shows only own pages
        let list1 = store.list_pages("agent-1").await.unwrap();
        assert_eq!(list1.len(), 1);
        assert_eq!(list1[0].title, "Secret");

        let list2 = store.list_pages("agent-2").await.unwrap();
        assert_eq!(list2.len(), 1);
        assert_eq!(list2[0].title, "Other");
    }

    #[tokio::test]
    async fn test_fts_search_after_upsert() {
        let store = make_store();
        store
            .write_page("agent-1", "Evolving", "original content about cats", &[])
            .await
            .unwrap();
        // Upsert with new content
        store
            .write_page("agent-1", "Evolving", "updated content about dogs", &[])
            .await
            .unwrap();

        // Old content should NOT be findable (FTS5 update trigger must remove old entry)
        let old = store.search("agent-1", "cats", &[], 10).await.unwrap();
        assert!(old.is_empty());

        // New content SHOULD be findable
        let new = store.search("agent-1", "dogs", &[], 10).await.unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].page.title, "Evolving");
    }

    #[tokio::test]
    async fn test_fts_search_after_delete() {
        let store = make_store();
        store
            .write_page("agent-1", "Temporary", "findable content", &[])
            .await
            .unwrap();
        store.delete_page("agent-1", "Temporary").await.unwrap();

        // FTS5 delete trigger must remove the entry
        let results = store.search("agent-1", "findable", &[], 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_search_agent_isolation() {
        let store = make_store();
        store
            .write_page("agent-1", "Secret Plan", "classified information", &[])
            .await
            .unwrap();
        store
            .write_page("agent-2", "Public Info", "common knowledge", &[])
            .await
            .unwrap();

        // Agent 2 should NOT find agent 1's content via search
        let results = store
            .search("agent-2", "classified", &[], 10)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_backlinks_populated() {
        let store = make_store();

        store
            .write_page("agent-1", "Page B", "target", &[])
            .await
            .unwrap();
        store
            .write_page("agent-1", "Page A", "links to [[Page B]]", &[])
            .await
            .unwrap();

        let backlinks = store.get_backlinks("agent-1", "Page B").await.unwrap();
        assert_eq!(backlinks.len(), 1);
        assert_eq!(backlinks[0].title, "Page A");
    }

    #[tokio::test]
    async fn test_outlinks_populated() {
        let store = make_store();

        store
            .write_page("agent-1", "Page A", "[[B]] and [[C]]", &[])
            .await
            .unwrap();

        let outlinks = store.get_outlinks("agent-1", "Page A").await.unwrap();
        assert_eq!(outlinks, vec!["B".to_string(), "C".to_string()]);
    }

    #[tokio::test]
    async fn test_links_updated_on_rewrite() {
        let store = make_store();

        store
            .write_page("agent-1", "B", "target", &[])
            .await
            .unwrap();
        store
            .write_page("agent-1", "A", "points to [[B]]", &[])
            .await
            .unwrap();
        assert_eq!(store.get_backlinks("agent-1", "B").await.unwrap().len(), 1);

        store
            .write_page("agent-1", "A", "rewritten without links", &[])
            .await
            .unwrap();
        assert!(store
            .get_backlinks("agent-1", "B")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_delete_cleans_outlinks() {
        let store = make_store();

        let a = store
            .write_page("agent-1", "A", "points to [[B]] and [[C]]", &[])
            .await
            .unwrap();

        store.delete_page("agent-1", "A").await.unwrap();

        let db = store.conn.lock().unwrap();
        let remaining: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM link_index WHERE source_id = ?1",
                params![&a.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn test_delete_preserves_inbound() {
        let store = make_store();

        store
            .write_page("agent-1", "Page B", "target page", &[])
            .await
            .unwrap();
        store
            .write_page("agent-1", "Page A", "links to [[Page B]]", &[])
            .await
            .unwrap();

        store.delete_page("agent-1", "Page B").await.unwrap();

        let outlinks = store.get_outlinks("agent-1", "Page A").await.unwrap();
        assert_eq!(outlinks, vec!["Page B".to_string()]);
        let backlinks = store.get_backlinks("agent-1", "Page B").await.unwrap();
        assert_eq!(backlinks.len(), 1);
        assert_eq!(backlinks[0].title, "Page A");
    }

    #[tokio::test]
    async fn test_orphan_detection() {
        let store = make_store();

        store
            .write_page("agent-1", "A", "links to [[B]]", &[])
            .await
            .unwrap();
        store
            .write_page("agent-1", "B", "linked target", &[])
            .await
            .unwrap();
        store
            .write_page("agent-1", "C", "independent page", &[])
            .await
            .unwrap();

        let orphans = store.get_orphans("agent-1").await.unwrap();
        let orphan_titles: Vec<String> = orphans.into_iter().map(|p| p.title).collect();
        assert!(orphan_titles.contains(&"A".to_string()));
        assert!(orphan_titles.contains(&"C".to_string()));
        assert!(!orphan_titles.contains(&"B".to_string()));
    }

    #[tokio::test]
    async fn test_unresolved_links() {
        let store = make_store();

        store
            .write_page("agent-1", "A", "has [[NonExistent]] target", &[])
            .await
            .unwrap();

        let info = store.get_all_links("agent-1", "A").await.unwrap();
        assert_eq!(info.outlinks, vec!["NonExistent".to_string()]);
        assert_eq!(info.unresolved, vec!["NonExistent".to_string()]);
    }

    #[tokio::test]
    async fn test_page_count() {
        let store = make_store();

        assert_eq!(store.page_count("agent-1").await.unwrap(), 0);

        store.write_page("agent-1", "P1", "c1", &[]).await.unwrap();
        store.write_page("agent-1", "P2", "c2", &[]).await.unwrap();

        assert_eq!(store.page_count("agent-1").await.unwrap(), 2);
        assert_eq!(store.page_count("agent-2").await.unwrap(), 0);
    }

    // ─── Cross-agent store tests ───

    #[tokio::test]
    async fn test_cross_agent_read_isolation() {
        let store = make_store();

        store
            .write_page("agent-1", "Secret", "Agent 1 secrets", &[])
            .await
            .unwrap();

        // Agent 2 cannot read agent 1's page (PageNotFound)
        let result = store.read_page("agent-2", "Secret").await;
        assert!(matches!(
            result.unwrap_err(),
            ScratchError::PageNotFound { .. }
        ));

        // Agent 1 can read their own page
        let page = store.read_page("agent-1", "Secret").await.unwrap();
        assert_eq!(page.content, "Agent 1 secrets");
    }

    #[tokio::test]
    async fn test_cross_agent_read_with_store() {
        let store = make_store();

        store
            .write_page("agent-1", "Shared Info", "Useful data", &[])
            .await
            .unwrap();

        // Cross-agent read: use agent-1's agent_id to read their page
        let page = store.read_page("agent-1", "Shared Info").await.unwrap();
        assert_eq!(page.agent_id, "agent-1");
        assert_eq!(page.content, "Useful data");
    }

    #[tokio::test]
    async fn test_get_outlinks_detailed_with_cross_agent() {
        let store = make_store();

        // Page with both local and cross-agent links
        store
            .write_page(
                "agent-1",
                "Hub",
                "See [[Local Page]] and [[@agent-2/Remote Page]].",
                &[],
            )
            .await
            .unwrap();

        let outlinks = store.get_outlinks_detailed("agent-1", "Hub").await.unwrap();

        assert_eq!(outlinks.len(), 2);

        // Find local link
        let local = outlinks.iter().find(|o| o.target_title == "Local Page");
        assert!(local.is_some());
        let local = local.unwrap();
        assert!(!local.is_cross_agent);
        assert!(local.target_agent_id.is_none());

        // Find cross-agent link
        let cross = outlinks.iter().find(|o| o.target_title == "Remote Page");
        assert!(cross.is_some());
        let cross = cross.unwrap();
        assert!(cross.is_cross_agent);
        assert_eq!(cross.target_agent_id.as_deref(), Some("agent-2"));
    }

    #[tokio::test]
    async fn test_cross_agent_search_isolation() {
        let store = make_store();

        store
            .write_page("agent-1", "Rust Tips", "How to write safe Rust code", &[])
            .await
            .unwrap();
        store
            .write_page("agent-2", "Rust Tricks", "Advanced Rust patterns", &[])
            .await
            .unwrap();

        // Search scoped to agent-1
        let results = store.search("agent-1", "Rust", &[], 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.agent_id, "agent-1");

        // Search scoped to agent-2
        let results = store.search("agent-2", "Rust", &[], 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page.agent_id, "agent-2");
    }

    #[tokio::test]
    async fn test_cross_agent_duplicate_title_both_stored() {
        let store = make_store();

        // Page links to BOTH a local "Bug Report" and a cross-agent "Bug Report"
        store
            .write_page(
                "agent-1",
                "Hub",
                "Local [[Bug Report]] and cross-agent [[@agent-2/Bug Report]].",
                &[],
            )
            .await
            .unwrap();

        let outlinks = store.get_outlinks_detailed("agent-1", "Hub").await.unwrap();

        // Both links must be stored (UNIQUE constraint includes target_agent_id)
        assert_eq!(outlinks.len(), 2);

        let local = outlinks
            .iter()
            .find(|o| !o.is_cross_agent && o.target_title == "Bug Report");
        assert!(local.is_some(), "Local link to Bug Report missing");

        let cross = outlinks
            .iter()
            .find(|o| o.is_cross_agent && o.target_title == "Bug Report");
        assert!(cross.is_some(), "Cross-agent link to Bug Report missing");
        assert_eq!(cross.unwrap().target_agent_id.as_deref(), Some("agent-2"));
    }

    #[tokio::test]
    async fn test_cross_agent_link_index_tracking() {
        let store = make_store();

        // Page with cross-agent link
        store
            .write_page(
                "agent-1",
                "Notes",
                "Reference: [[@agent-2/Bug Report]]",
                &[],
            )
            .await
            .unwrap();

        // Outlinks should include the cross-agent link
        let outlinks = store.get_outlinks("agent-1", "Notes").await.unwrap();
        assert!(outlinks.contains(&"Bug Report".to_string()));

        // Backlinks query filters out cross-agent links (same-agent only)
        let backlinks = store.get_backlinks("agent-1", "Bug Report").await.unwrap();
        assert!(backlinks.is_empty()); // No same-agent backlinks
    }
}
