//! SQLite-backed store for proactive recommendations (Phase 4).
//!
//! Mirrors `workspace_store.rs` / `checkpoint_store.rs` conventions:
//! `Arc<Mutex<rusqlite::Connection>>`, all public methods are `async fn` and
//! run their DB work inside `tokio::task::spawn_blocking`, WAL pragmas on open,
//! and parameterized queries only (no string interpolation).
//!
//! Opened at kernel boot at `{data_dir}/recommendations.db` with fallback to an
//! in-memory connection on open failure (same policy `workspace_store` uses).
//! The rate-limit and dedup primitives live here so neither the engine nor any
//! future caller can bypass them.

use anyhow::Context;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ─── Domain types ────────────────────────────────────────────────────────────

/// The semantic category of a proactive recommendation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationKind {
    /// A short hint about a tool or pattern ("you parse CSVs often — try data-parser").
    Tip,
    /// A concrete suggestion tied to interests / profile.
    Recommendation,
}

impl RecommendationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RecommendationKind::Tip => "tip",
            RecommendationKind::Recommendation => "recommendation",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tip" => Some(RecommendationKind::Tip),
            "recommendation" => Some(RecommendationKind::Recommendation),
            _ => None,
        }
    }
}

/// Lifecycle status of a recommendation row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationStatus {
    /// Generated, not yet delivered.
    Pending,
    /// Pushed to the user inbox/channel.
    Delivered,
    /// User acted on it (Phase 5 reads this).
    Accepted,
    /// User explicitly rejected it (Phase 5 reads this).
    Dismissed,
    /// Aged out / superseded.
    Expired,
}

impl RecommendationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RecommendationStatus::Pending => "pending",
            RecommendationStatus::Delivered => "delivered",
            RecommendationStatus::Accepted => "accepted",
            RecommendationStatus::Dismissed => "dismissed",
            RecommendationStatus::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(RecommendationStatus::Pending),
            "delivered" => Some(RecommendationStatus::Delivered),
            "accepted" => Some(RecommendationStatus::Accepted),
            "dismissed" => Some(RecommendationStatus::Dismissed),
            "expired" => Some(RecommendationStatus::Expired),
            _ => None,
        }
    }
}

/// A persisted recommendation row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    /// UUID v4 string.
    pub id: String,
    pub kind: RecommendationKind,
    /// Short tip text shown to the user.
    pub content: String,
    /// JSON array of topic strings that drove this recommendation.
    pub basis: String,
    /// 0.0 ..= 1.0
    pub confidence: f64,
    pub status: RecommendationStatus,
    /// SHA-256 hash of `kind + "\n" + content.to_lowercase()`, first 8 bytes
    /// encoded as 16 hex chars. Used for dedup — production upgrades that had
    /// prior FNV-1a hashes stored should clear the table or let prune sweep
    /// remove them (old hashes will no longer match, causing one-time re-delivery).
    pub dedup_hash: String,
    /// Unix epoch seconds (UTC).
    pub created_at: i64,
    /// Set when the recommendation transitions to `Delivered`.
    pub delivered_at: Option<i64>,
    /// Set when the user provides feedback (accepted / dismissed).
    pub feedback_at: Option<i64>,
}

impl Recommendation {
    /// Compute a SHA-256 content-hash (first 16 hex chars) from `kind + content`.
    ///
    /// Using SHA-256 (already a workspace dependency via `sha2`) rather than
    /// a hand-rolled FNV-1a gives 2^64 collision resistance on the truncated
    /// output vs. 2^32 for FNV-1a, making accidental suppression of distinct
    /// recommendations negligible. The `sha2` crate is in scope for
    /// `agentos-kernel` via its workspace dep.
    pub fn compute_dedup_hash(kind: RecommendationKind, content: &str) -> String {
        use sha2::{Digest, Sha256};
        let input = format!("{}\n{}", kind.as_str(), content.to_lowercase());
        let digest = Sha256::digest(input.as_bytes());
        // Take the first 8 bytes (64 bits) as a hex string — matches the previous
        // 16-char width so existing stored hashes keep the same format length.
        let bytes: [u8; 8] = digest[..8].try_into().expect("sha256 output >= 8 bytes");
        format!("{:016x}", u64::from_be_bytes(bytes))
    }
}

// ─── Store ───────────────────────────────────────────────────────────────────

/// SQLite-backed store for `recommendations.db`.
pub struct RecommendationsStore {
    conn: Arc<Mutex<Connection>>,
}

impl RecommendationsStore {
    // ── Constructors ─────────────────────────────────────────────────────────

    /// Open (or create) the store at `path`, enabling WAL mode and running DDL.
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            let conn = Connection::open(&path)
                .with_context(|| format!("open recommendations.db at {}", path.display()))?;
            configure_connection(&conn)?;
            run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("spawn_blocking open recommendations.db")??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open an in-memory store — for tests and boot-fallback.
    pub async fn open_in_memory() -> anyhow::Result<Self> {
        let conn = tokio::task::spawn_blocking(|| -> anyhow::Result<Connection> {
            let conn = Connection::open_in_memory().context("open in-memory recommendations db")?;
            configure_connection(&conn)?;
            run_migrations(&conn)?;
            Ok(conn)
        })
        .await
        .context("spawn_blocking open in-memory recommendations.db")??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ── Write operations ──────────────────────────────────────────────────────

    /// Insert `rec`. Returns `true` if inserted, `false` if a row with the same
    /// `dedup_hash` already exists (INSERT OR IGNORE semantics).
    ///
    /// Also enforces a `confidence >= min_confidence` floor: returns `false` for
    /// below-floor candidates without returning an error (soft rejection).
    pub async fn insert(&self, rec: Recommendation, min_confidence: f64) -> anyhow::Result<bool> {
        if rec.confidence < min_confidence {
            return Ok(false);
        }
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let rows = guard.execute(
                "INSERT OR IGNORE INTO recommendations
                     (id, kind, content, basis, confidence, status, dedup_hash,
                      created_at, delivered_at, feedback_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    rec.id,
                    rec.kind.as_str(),
                    rec.content,
                    rec.basis,
                    rec.confidence,
                    rec.status.as_str(),
                    rec.dedup_hash,
                    rec.created_at,
                    rec.delivered_at,
                    rec.feedback_at,
                ],
            )?;
            Ok(rows > 0)
        })
        .await
        .context("spawn_blocking insert recommendation")?
    }

    /// Update status to `delivered` and set `delivered_at = at`.
    pub async fn mark_delivered(&self, id: &str, at: i64) -> anyhow::Result<()> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            guard.execute(
                "UPDATE recommendations SET status = 'delivered', delivered_at = ?1 WHERE id = ?2",
                params![at, id],
            )?;
            Ok(())
        })
        .await
        .context("spawn_blocking mark_delivered")?
    }

    /// Record user feedback: sets `status` to `accepted` or `dismissed` and
    /// `feedback_at = at`.
    pub async fn record_feedback(&self, id: &str, accepted: bool, at: i64) -> anyhow::Result<()> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        let status = if accepted { "accepted" } else { "dismissed" };
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            guard.execute(
                "UPDATE recommendations SET status = ?1, feedback_at = ?2 WHERE id = ?3",
                params![status, at, id],
            )?;
            Ok(())
        })
        .await
        .context("spawn_blocking record_feedback")?
    }

    /// Delete all rows with `created_at < cutoff_unix`. Returns the number of
    /// rows removed.
    pub async fn prune_older_than(&self, cutoff_unix: i64) -> anyhow::Result<u64> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let n = guard.execute(
                "DELETE FROM recommendations WHERE created_at < ?1",
                params![cutoff_unix],
            )?;
            Ok(n as u64)
        })
        .await
        .context("spawn_blocking prune_older_than")?
    }

    /// Delete a single row by id. Returns true if a row was removed.
    ///
    /// Used to roll back a pending recommendation row when delivery fails so the
    /// dedup slot is not silently consumed for the full cooldown window.
    pub async fn delete_by_id(&self, id: &str) -> anyhow::Result<bool> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let n = guard.execute("DELETE FROM recommendations WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
        .await
        .context("spawn_blocking delete_by_id recommendation")?
    }

    /// Delete **all** rows. Returns the count of deleted rows (right-to-forget).
    pub async fn clear_all(&self) -> anyhow::Result<usize> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let n = guard.execute("DELETE FROM recommendations", [])?;
            Ok(n)
        })
        .await
        .context("spawn_blocking clear_all")?
    }

    // ── Read / query operations ───────────────────────────────────────────────

    /// Count rows with `delivered_at >= since_unix`. Used for the daily
    /// rate-limit check.
    pub async fn count_delivered_since(&self, since_unix: i64) -> anyhow::Result<u32> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<u32> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let n: i64 = guard.query_row(
                "SELECT COUNT(*) FROM recommendations
                 WHERE delivered_at IS NOT NULL AND delivered_at >= ?1",
                params![since_unix],
                |row| row.get(0),
            )?;
            Ok(n as u32)
        })
        .await
        .context("spawn_blocking count_delivered_since")?
    }

    /// Returns `true` if a row with `dedup_hash` exists whose `created_at` is
    /// within `cooldown_secs` of `now`.
    ///
    /// This suppresses identical recommendations within the configured cooldown
    /// window regardless of their current status.
    pub async fn is_on_cooldown(
        &self,
        dedup_hash: &str,
        now: i64,
        cooldown_secs: i64,
    ) -> anyhow::Result<bool> {
        let conn = Arc::clone(&self.conn);
        let hash = dedup_hash.to_string();
        let since = now - cooldown_secs;
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let n: i64 = guard.query_row(
                "SELECT COUNT(*) FROM recommendations
                 WHERE dedup_hash = ?1 AND created_at >= ?2",
                params![hash, since],
                |row| row.get(0),
            )?;
            Ok(n > 0)
        })
        .await
        .context("spawn_blocking is_on_cooldown")?
    }

    /// List all recommendations ordered by `created_at DESC` (newest first).
    ///
    /// Convenience wrapper over [`Self::list_filtered`] with no status filter.
    pub async fn list(&self, limit: u32) -> anyhow::Result<Vec<Recommendation>> {
        self.list_filtered(None, limit).await
    }

    /// List recommendations, optionally filtered by status, ordered by
    /// `created_at DESC`.
    pub async fn list_filtered(
        &self,
        status: Option<RecommendationStatus>,
        limit: u32,
    ) -> anyhow::Result<Vec<Recommendation>> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Recommendation>> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

            let rows = if let Some(s) = status {
                let mut stmt = guard.prepare(
                    "SELECT id, kind, content, basis, confidence, status, dedup_hash,
                             created_at, delivered_at, feedback_at
                     FROM recommendations
                     WHERE status = ?1
                     ORDER BY created_at DESC
                     LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map(params![s.as_str(), limit], map_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            } else {
                let mut stmt = guard.prepare(
                    "SELECT id, kind, content, basis, confidence, status, dedup_hash,
                             created_at, delivered_at, feedback_at
                     FROM recommendations
                     ORDER BY created_at DESC
                     LIMIT ?1",
                )?;
                let rows = stmt
                    .query_map(params![limit], map_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };
            Ok(rows)
        })
        .await
        .context("spawn_blocking list recommendations")?
    }

    /// Fetch a single recommendation by id.
    pub async fn get(&self, id: &str) -> anyhow::Result<Option<Recommendation>> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Recommendation>> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let mut stmt = guard.prepare(
                "SELECT id, kind, content, basis, confidence, status, dedup_hash,
                         created_at, delivered_at, feedback_at
                 FROM recommendations WHERE id = ?1",
            )?;
            let mut rows = stmt.query_map(params![id], map_row)?;
            match rows.next() {
                Some(row) => Ok(Some(row?)),
                None => Ok(None),
            }
        })
        .await
        .context("spawn_blocking get recommendation")?
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .context("configure recommendations db pragmas")?;
    Ok(())
}

fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS recommendations (
            id           TEXT    PRIMARY KEY,
            kind         TEXT    NOT NULL,
            content      TEXT    NOT NULL,
            basis        TEXT    NOT NULL,
            confidence   REAL    NOT NULL,
            status       TEXT    NOT NULL DEFAULT 'pending'
                         CHECK (status IN ('pending','delivered','accepted','dismissed','expired')),
            dedup_hash   TEXT    NOT NULL,
            created_at   INTEGER NOT NULL,
            delivered_at INTEGER,
            feedback_at  INTEGER
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_rec_dedup
            ON recommendations(dedup_hash);
        CREATE INDEX IF NOT EXISTS idx_rec_status
            ON recommendations(status, created_at);
        CREATE INDEX IF NOT EXISTS idx_rec_created
            ON recommendations(created_at);
        CREATE INDEX IF NOT EXISTS idx_rec_delivered
            ON recommendations(delivered_at);",
    )
    .context("run recommendations db migrations")?;
    Ok(())
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Recommendation> {
    let kind_str: String = row.get(1)?;
    let status_str: String = row.get(5)?;
    Ok(Recommendation {
        id: row.get(0)?,
        kind: RecommendationKind::parse(&kind_str).unwrap_or(RecommendationKind::Tip),
        content: row.get(2)?,
        basis: row.get(3)?,
        confidence: row.get(4)?,
        status: RecommendationStatus::parse(&status_str).unwrap_or(RecommendationStatus::Pending),
        dedup_hash: row.get(6)?,
        created_at: row.get(7)?,
        delivered_at: row.get(8)?,
        feedback_at: row.get(9)?,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rec(
        kind: RecommendationKind,
        content: &str,
        confidence: f64,
        created_at: i64,
    ) -> Recommendation {
        let dedup_hash = Recommendation::compute_dedup_hash(kind, content);
        Recommendation {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            content: content.to_string(),
            basis: r#"["test"]"#.to_string(),
            confidence,
            status: RecommendationStatus::Pending,
            dedup_hash,
            created_at,
            delivered_at: None,
            feedback_at: None,
        }
    }

    async fn store() -> RecommendationsStore {
        RecommendationsStore::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn dedup_prevents_re_insert() {
        let s = store().await;
        let rec = make_rec(
            RecommendationKind::Tip,
            "Try the data-parser tool",
            0.8,
            1000,
        );
        let inserted = s.insert(rec.clone(), 0.5).await.unwrap();
        assert!(inserted, "first insert should succeed");

        // Same dedup_hash (same kind + content) — should be ignored.
        let dup = Recommendation {
            id: uuid::Uuid::new_v4().to_string(), // different id, same hash
            ..rec
        };
        let re_inserted = s.insert(dup, 0.5).await.unwrap();
        assert!(!re_inserted, "duplicate insert should return false");

        let all = s.list(100).await.unwrap();
        assert_eq!(all.len(), 1, "only one row should exist after dedup");
    }

    #[tokio::test]
    async fn confidence_floor_rejects_low_confidence() {
        let s = store().await;
        let rec = make_rec(RecommendationKind::Tip, "Low confidence tip", 0.3, 1000);
        let inserted = s.insert(rec, 0.5).await.unwrap();
        assert!(!inserted, "below-floor recommendation should be rejected");

        let all = s.list(100).await.unwrap();
        assert!(
            all.is_empty(),
            "no rows should be inserted for low-confidence rec"
        );
    }

    #[tokio::test]
    async fn rate_limit_count_works() {
        let s = store().await;
        let now = 1_000_000i64;
        let day_ago = now - 86_400;

        // Two delivered within the last 24h.
        for i in 0..2 {
            let mut rec = make_rec(RecommendationKind::Tip, &format!("tip {i}"), 0.8, now - i);
            rec.dedup_hash = format!("hash{i:016}"); // unique hashes
            s.insert(rec, 0.5).await.unwrap();
            s.mark_delivered(&format!("hash{i:016}"), now - i)
                .await
                .ok(); // mark by id not hash
        }

        // Insert via id properly.
        let s2 = store().await;
        let rec1 = Recommendation {
            id: "id-a".to_string(),
            kind: RecommendationKind::Tip,
            content: "tip A".to_string(),
            basis: "[]".to_string(),
            confidence: 0.8,
            status: RecommendationStatus::Pending,
            dedup_hash: "hashAAA0000000000".to_string(),
            created_at: now,
            delivered_at: Some(now),
            feedback_at: None,
        };
        let rec2 = Recommendation {
            id: "id-b".to_string(),
            kind: RecommendationKind::Tip,
            content: "tip B".to_string(),
            basis: "[]".to_string(),
            confidence: 0.8,
            status: RecommendationStatus::Pending,
            dedup_hash: "hashBBB0000000000".to_string(),
            created_at: now,
            delivered_at: Some(now),
            feedback_at: None,
        };
        // Old delivery (> 24h ago).
        let rec3 = Recommendation {
            id: "id-c".to_string(),
            kind: RecommendationKind::Tip,
            content: "tip C (old)".to_string(),
            basis: "[]".to_string(),
            confidence: 0.8,
            status: RecommendationStatus::Delivered,
            dedup_hash: "hashCCC0000000000".to_string(),
            created_at: day_ago - 1,
            delivered_at: Some(day_ago - 1), // outside 24h window
            feedback_at: None,
        };
        s2.insert(rec1, 0.5).await.unwrap();
        s2.insert(rec2, 0.5).await.unwrap();
        s2.insert(rec3, 0.5).await.unwrap();

        // Count within last 24h window (since = now - 86400).
        let since = now - 86_400;
        let count = s2.count_delivered_since(since).await.unwrap();
        assert_eq!(count, 2, "only the 2 recent deliveries should count");
    }

    #[tokio::test]
    async fn is_on_cooldown_within_and_outside() {
        let s = store().await;
        let now = 2_000_000i64;
        let cooldown_secs = 7 * 24 * 3600i64; // 7 days

        let rec = Recommendation {
            id: "cool-id".to_string(),
            kind: RecommendationKind::Tip,
            content: "cool tip".to_string(),
            basis: "[]".to_string(),
            confidence: 0.9,
            status: RecommendationStatus::Delivered,
            dedup_hash: "cooldeduphhash0000".to_string(),
            created_at: now - 100, // very recent
            delivered_at: Some(now - 100),
            feedback_at: None,
        };
        s.insert(rec, 0.5).await.unwrap();

        // Within cooldown.
        let on = s
            .is_on_cooldown("cooldeduphhash0000", now, cooldown_secs)
            .await
            .unwrap();
        assert!(on, "should be on cooldown within window");

        // Outside cooldown (now advanced past the window).
        let far_future = now + cooldown_secs + 1;
        let off = s
            .is_on_cooldown("cooldeduphhash0000", far_future, cooldown_secs)
            .await
            .unwrap();
        assert!(!off, "should not be on cooldown outside window");
    }

    #[tokio::test]
    async fn feedback_persists() {
        let s = store().await;
        let rec = make_rec(
            RecommendationKind::Recommendation,
            "Consider learning Rust",
            0.85,
            1000,
        );
        let id = rec.id.clone();
        s.insert(rec, 0.5).await.unwrap();

        s.record_feedback(&id, false, 2000).await.unwrap(); // dismissed
        let fetched = s.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.status.as_str(), "dismissed");
        assert_eq!(fetched.feedback_at, Some(2000));

        s.record_feedback(&id, true, 3000).await.unwrap(); // accepted
        let fetched = s.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.status.as_str(), "accepted");
    }

    #[tokio::test]
    async fn prune_old_recommendations() {
        let s = store().await;
        let old_ts = 500i64;
        let new_ts = 1_000_000i64;

        let old_rec = Recommendation {
            id: "old-id".to_string(),
            kind: RecommendationKind::Tip,
            content: "old tip".to_string(),
            basis: "[]".to_string(),
            confidence: 0.8,
            status: RecommendationStatus::Delivered,
            dedup_hash: "olddeduphhash0000".to_string(),
            created_at: old_ts,
            delivered_at: Some(old_ts),
            feedback_at: None,
        };
        let new_rec = Recommendation {
            id: "new-id".to_string(),
            kind: RecommendationKind::Tip,
            content: "new tip".to_string(),
            basis: "[]".to_string(),
            confidence: 0.8,
            status: RecommendationStatus::Pending,
            dedup_hash: "newdeduphhash0000".to_string(),
            created_at: new_ts,
            delivered_at: None,
            feedback_at: None,
        };
        s.insert(old_rec, 0.5).await.unwrap();
        s.insert(new_rec, 0.5).await.unwrap();

        let pruned = s.prune_older_than(new_ts - 1).await.unwrap();
        assert_eq!(pruned, 1, "should prune exactly 1 old row");

        let remaining = s.list(100).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "new-id");
    }
}
