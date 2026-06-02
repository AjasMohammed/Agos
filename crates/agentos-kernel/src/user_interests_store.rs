//! Persistent store for the proactive-personalization interest model.
//!
//! Mirrors the `user_pref_proposals.rs` / `tool_usage_store.rs` SQLite conventions:
//! `conn: Arc<std::sync::Mutex<Connection>>`, all public methods are `async` and run
//! their DB work inside `tokio::task::spawn_blocking`, parameterized queries only,
//! RFC3339 TEXT timestamps via `chrono`.
//!
//! Decayed scores are **not** stored — they are derived on read from `weight`,
//! `half_life_hours`, and `now - last_reinforced`. `reinforce` is a decay-then-add
//! accumulator so a topic reinforced repeatedly over time keeps a sensible weight.
//!
//! This store is **zero task-context cost**: it is only ever touched by the
//! background [`crate::interest_model::InterestModel`] aggregator and the Phase 4
//! recommendation engine — never injected into a `ContextWindow`.

use anyhow::Context;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Type of behavioral signal a topic was reinforced by.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignalType {
    /// Keyword mined from a successful task / episode summary.
    TaskTopic,
    /// A tool that was actually exercised (decayed usage rank).
    ToolUsage,
    /// Activity on a particular channel.
    ChannelActivity,
    /// Explicitly declared interest.
    Explicit,
}

impl SignalType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SignalType::TaskTopic => "task_topic",
            SignalType::ToolUsage => "tool_usage",
            SignalType::ChannelActivity => "channel_activity",
            SignalType::Explicit => "explicit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "task_topic" => Some(SignalType::TaskTopic),
            "tool_usage" => Some(SignalType::ToolUsage),
            "channel_activity" => Some(SignalType::ChannelActivity),
            "explicit" => Some(SignalType::Explicit),
            _ => None,
        }
    }
}

/// A persisted interest topic row (raw, undecayed weight).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterestTopic {
    pub topic: String,
    pub signal_type: SignalType,
    pub weight: f64,
    pub last_reinforced: chrono::DateTime<chrono::Utc>,
    pub half_life_hours: f64,
}

/// A query result carrying the decayed score at the query instant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterestScore {
    pub topic: String,
    pub score: f64,
    pub signal_type: SignalType,
}

/// Exponential half-life decay of `weight` over `age_hours` (hours).
///
/// `0.5^(age/half_life)`. Clamps negative ages (clock skew / same instant) to no
/// decay, and a non-positive `half_life_hours` to no decay.
pub fn decay(weight: f64, age_hours: f64, half_life_hours: f64) -> f64 {
    if half_life_hours <= 0.0 {
        return weight;
    }
    if age_hours <= 0.0 {
        return weight;
    }
    weight * 0.5_f64.powf(age_hours / half_life_hours)
}

pub struct UserInterestsStore {
    conn: Arc<Mutex<Connection>>,
}

impl UserInterestsStore {
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            let conn = Connection::open(&path)
                .with_context(|| format!("open user_interests.db at {}", path.display()))?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
            conn.execute_batch(Self::DDL_TABLE_ONLY)?;
            Ok(conn)
        })
        .await
        .context("spawn_blocking open user_interests")??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open a transient in-memory store (used when `personalization.enabled = false`
    /// so no DB files are created on disk for a feature the operator has opted out of).
    ///
    /// Runs the DDL inside `spawn_blocking` so it respects the codebase's invariant
    /// that SQLite I/O never runs directly on the Tokio async executor thread.
    pub async fn open_in_memory() -> anyhow::Result<Self> {
        let conn = tokio::task::spawn_blocking(|| -> anyhow::Result<Connection> {
            let conn = Connection::open_in_memory().context("open user_interests in-memory")?;
            conn.execute_batch(Self::DDL_TABLE_ONLY)
                .context("user_interests in-memory DDL")?;
            Ok(conn)
        })
        .await
        .context("spawn_blocking open user_interests in-memory")??;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// DDL run via `execute_batch` in both `open()` and `open_in_memory()`.
    /// Does NOT include the WAL pragma — that is set separately in the file-backed
    /// path (WAL mode is silently ignored for :memory: connections anyway).
    const DDL_TABLE_ONLY: &'static str = "
        CREATE TABLE IF NOT EXISTS interest_topics (
            topic           TEXT PRIMARY KEY,
            signal_type     TEXT NOT NULL,
            weight          REAL NOT NULL,
            last_reinforced TEXT NOT NULL,
            half_life_hours REAL NOT NULL
        );
    ";

    /// Exact row count from `interest_topics` (all rows, regardless of decay).
    /// Used by `personalization status` for an accurate count without loading and
    /// decaying every row the way `top_interests(u32::MAX)` would.
    pub async fn count_topics(&self) -> anyhow::Result<usize> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let n: i64 =
                guard.query_row("SELECT COUNT(*) FROM interest_topics", [], |r| r.get(0))?;
            Ok(n as usize)
        })
        .await
        .context("spawn_blocking count interest_topics")?
    }

    /// Load ALL rows as raw (undecayed) signals — used by `personalization export`
    /// so the dump is complete regardless of decay state.
    pub async fn load_all_raw(&self) -> anyhow::Result<Vec<InterestTopic>> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<InterestTopic>> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let mut stmt = guard.prepare(
                "SELECT topic, signal_type, weight, last_reinforced, half_life_hours
                 FROM interest_topics ORDER BY topic",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (topic, sig_str, weight, last_str, hl) = row?;
                let signal_type = SignalType::parse(&sig_str).unwrap_or(SignalType::TaskTopic);
                let last_reinforced = chrono::DateTime::parse_from_rfc3339(&last_str)
                    .map(|d| d.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
                out.push(InterestTopic {
                    topic,
                    signal_type,
                    weight,
                    last_reinforced,
                    half_life_hours: hl,
                });
            }
            Ok(out)
        })
        .await
        .context("spawn_blocking load_all_raw interest_topics")?
    }

    /// Reinforce a topic: decay the existing weight to `now` first, then add `delta`.
    ///
    /// Decay is computed in Rust (SQLite has no native `pow`). UPSERT on the topic
    /// primary key with `last_reinforced = now`.
    pub async fn reinforce(
        &self,
        topic: &str,
        signal_type: SignalType,
        delta: f64,
        half_life_hours: f64,
    ) -> anyhow::Result<()> {
        let conn = Arc::clone(&self.conn);
        let topic = topic.to_string();
        let signal = signal_type.as_str().to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let now = chrono::Utc::now();
            // Decay-then-add accumulator.
            let existing: Option<(f64, String)> = guard
                .query_row(
                    "SELECT weight, last_reinforced FROM interest_topics WHERE topic = ?1",
                    params![topic],
                    |r| Ok((r.get::<_, f64>(0)?, r.get::<_, String>(1)?)),
                )
                .ok();
            let new_weight = match existing {
                Some((old_weight, last_str)) => {
                    let last = chrono::DateTime::parse_from_rfc3339(&last_str)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .unwrap_or(now);
                    let age_hours =
                        (now - last).num_seconds().max(0) as f64 / 3600.0;
                    decay(old_weight, age_hours, half_life_hours) + delta
                }
                None => delta,
            };
            guard.execute(
                "INSERT INTO interest_topics (topic, signal_type, weight, last_reinforced, half_life_hours)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(topic) DO UPDATE SET
                    signal_type     = excluded.signal_type,
                    weight          = excluded.weight,
                    last_reinforced = excluded.last_reinforced,
                    half_life_hours = excluded.half_life_hours",
                params![topic, signal, new_weight, now.to_rfc3339(), half_life_hours],
            )?;
            Ok(())
        })
        .await
        .context("spawn_blocking reinforce interest")?
    }

    /// Decayed score for a single topic at `now`, or `None` if the topic is absent.
    pub async fn decayed_score(
        &self,
        topic: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Option<f64>> {
        let conn = Arc::clone(&self.conn);
        let topic = topic.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<f64>> {
            let guard = conn.lock().map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let row: Option<(f64, String, f64)> = guard
                .query_row(
                    "SELECT weight, last_reinforced, half_life_hours FROM interest_topics WHERE topic = ?1",
                    params![topic],
                    |r| {
                        Ok((
                            r.get::<_, f64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, f64>(2)?,
                        ))
                    },
                )
                .ok();
            Ok(row.map(|(weight, last_str, hl)| {
                let last = chrono::DateTime::parse_from_rfc3339(&last_str)
                    .map(|d| d.with_timezone(&chrono::Utc))
                    .unwrap_or(now);
                let age_hours = (now - last).num_seconds().max(0) as f64 / 3600.0;
                decay(weight, age_hours, hl)
            }))
        })
        .await
        .context("spawn_blocking decayed_score interest")?
    }

    /// Top `limit` topics by decayed score (desc). Scores are computed in Rust and
    /// rows with a non-positive decayed score are dropped. Callers may further
    /// filter by a minimum score.
    pub async fn top_interests(&self, limit: u32) -> anyhow::Result<Vec<InterestScore>> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<InterestScore>> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let now = chrono::Utc::now();
            let mut stmt = guard.prepare(
                "SELECT topic, signal_type, weight, last_reinforced, half_life_hours
                 FROM interest_topics",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })?;
            let mut scored: Vec<InterestScore> = Vec::new();
            for row in rows {
                let (topic, sig_str, weight, last_str, hl) = row?;
                let signal_type = SignalType::parse(&sig_str).unwrap_or(SignalType::TaskTopic);
                let last = chrono::DateTime::parse_from_rfc3339(&last_str)
                    .map(|d| d.with_timezone(&chrono::Utc))
                    .unwrap_or(now);
                let age_hours = (now - last).num_seconds().max(0) as f64 / 3600.0;
                let score = decay(weight, age_hours, hl);
                if score > 0.0 {
                    scored.push(InterestScore {
                        topic,
                        score,
                        signal_type,
                    });
                }
            }
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(limit as usize);
            Ok(scored)
        })
        .await
        .context("spawn_blocking top_interests")?
    }

    /// Delete topics whose decayed score at `now` is below `min_score`. Returns the
    /// number of rows pruned.
    pub async fn prune_below(&self, min_score: f64) -> anyhow::Result<usize> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let now = chrono::Utc::now();
            let mut to_delete: Vec<String> = Vec::new();
            {
                let mut stmt = guard.prepare(
                    "SELECT topic, weight, last_reinforced, half_life_hours FROM interest_topics",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, f64>(3)?,
                    ))
                })?;
                for row in rows {
                    let (topic, weight, last_str, hl) = row?;
                    let last = chrono::DateTime::parse_from_rfc3339(&last_str)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .unwrap_or(now);
                    let age_hours = (now - last).num_seconds().max(0) as f64 / 3600.0;
                    if decay(weight, age_hours, hl) < min_score {
                        to_delete.push(topic);
                    }
                }
            }
            if to_delete.is_empty() {
                return Ok(0);
            }
            // Batch all deletes in one statement so we hold the mutex for one
            // round-trip instead of N, reducing contention with concurrent
            // reinforce / top_interests calls.
            // Build `?1, ?2, …, ?N` placeholders — no string interpolation of
            // values, only safe positional binding via rusqlite's params_from_iter.
            let placeholders: String = (1..=to_delete.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!("DELETE FROM interest_topics WHERE topic IN ({placeholders})");
            let pruned = guard.execute(&sql, rusqlite::params_from_iter(to_delete.iter()))?;
            Ok(pruned)
        })
        .await
        .context("spawn_blocking prune_below interest")?
    }

    /// Delete all interest rows (Phase 6 right-to-forget). Returns rows removed.
    pub async fn clear_all(&self) -> anyhow::Result<usize> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let n = guard.execute("DELETE FROM interest_topics", [])?;
            Ok(n)
        })
        .await
        .context("spawn_blocking clear_all interest")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn store() -> UserInterestsStore {
        let dir = tempdir().unwrap();
        let path = dir.path().join("user_interests.db");
        let s = UserInterestsStore::open(path).await.unwrap();
        // Keep the tempdir alive for the test's lifetime (per-test fresh dir).
        Box::leak(Box::new(dir));
        s
    }

    #[test]
    fn decay_math_anchors() {
        // one half-life -> 0.5x
        assert!((decay(1.0, 168.0, 168.0) - 0.5).abs() < 1e-9);
        // two half-lives -> 0.25x
        assert!((decay(1.0, 336.0, 168.0) - 0.25).abs() < 1e-9);
        // zero age -> unchanged
        assert!((decay(1.0, 0.0, 168.0) - 1.0).abs() < 1e-9);
        // negative age clamps to unchanged
        assert!((decay(1.0, -5.0, 168.0) - 1.0).abs() < 1e-9);
        // non-positive half-life -> unchanged
        assert!((decay(1.0, 100.0, 0.0) - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn reinforce_then_decay() {
        let s = store().await;
        // Reinforce once with weight 1.0, half-life 168h.
        s.reinforce("rust", SignalType::TaskTopic, 1.0, 168.0)
            .await
            .unwrap();
        // Immediately, decayed score ~ 1.0.
        let now = chrono::Utc::now();
        let immediate = s.decayed_score("rust", now).await.unwrap().unwrap();
        assert!((immediate - 1.0).abs() < 1e-3, "immediate={immediate}");
        // After one half-life it should be ~ half.
        let later = now + chrono::Duration::hours(168);
        let decayed = s.decayed_score("rust", later).await.unwrap().unwrap();
        assert!(
            (decayed - 0.5).abs() < 1e-2,
            "after one half-life expected ~0.5, got {decayed}"
        );
    }

    #[tokio::test]
    async fn top_interests_sorted_and_filtered() {
        let s = store().await;
        s.reinforce("alpha", SignalType::TaskTopic, 3.0, 168.0)
            .await
            .unwrap();
        s.reinforce("beta", SignalType::ToolUsage, 1.0, 168.0)
            .await
            .unwrap();
        s.reinforce("gamma", SignalType::TaskTopic, 2.0, 168.0)
            .await
            .unwrap();
        let top = s.top_interests(2).await.unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].topic, "alpha");
        assert_eq!(top[1].topic, "gamma");
        assert!(top[0].score >= top[1].score);
    }

    #[tokio::test]
    async fn prune_below_removes_negligible() {
        let s = store().await;
        // Strong, recent.
        s.reinforce("keep", SignalType::TaskTopic, 5.0, 168.0)
            .await
            .unwrap();
        // Weak; use a tiny half-life so a fresh write is already tiny? No — fresh
        // writes start at full weight. Instead, write a small weight and prune at a
        // higher floor.
        s.reinforce("drop", SignalType::TaskTopic, 0.001, 168.0)
            .await
            .unwrap();
        let pruned = s.prune_below(0.05).await.unwrap();
        assert_eq!(pruned, 1);
        let now = chrono::Utc::now();
        assert!(s.decayed_score("drop", now).await.unwrap().is_none());
        assert!(s.decayed_score("keep", now).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn clear_all_empties_store() {
        let s = store().await;
        s.reinforce("x", SignalType::TaskTopic, 1.0, 168.0)
            .await
            .unwrap();
        s.reinforce("y", SignalType::ToolUsage, 1.0, 168.0)
            .await
            .unwrap();
        let removed = s.clear_all().await.unwrap();
        assert_eq!(removed, 2);
        assert!(s.top_interests(10).await.unwrap().is_empty());
    }
}
