//! Durable, structured user-profile store (`user_profile.db`).
//!
//! This is the canonical store all Proactive Personalization phases read from.
//! Accepted user-preference *proposals* (see [`crate::user_pref_proposals`]) are
//! promoted into this store as structured [`ProfileEntry`] rows with a category,
//! confidence, pin ordering, and usage tracking.
//!
//! Conventions mirror [`crate::user_pref_proposals`] exactly: an
//! `Arc<Mutex<Connection>>`, every public method is `async fn` returning
//! `anyhow::Result<T>` and runs its SQLite work inside `spawn_blocking`, WAL
//! pragmas on open, and parameterized queries only (no string interpolation).
//!
//! Every mutation bumps a monotonic `version` counter (in the same transaction
//! as the change) so [Phase 2 read-back](../../obsidian-vault/plans/proactive-personalization)
//! can gate prompt-cache invalidation on a cheap version read.

use agentos_types::{
    ProfileCategory, ProfileEntry, ProfileEntryStatus, ProfilePatch, ProfileSource,
};
use anyhow::Context;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Default confidence floor for insertion. Entries below the store's effective
/// floor are rejected (fail-closed, never clamped) so callers cannot smuggle
/// low-confidence guesses into L0. Operators may override via config; the
/// effective floor is `UserProfileStore::min_confidence`.
pub const MIN_INSERT_CONFIDENCE: f32 = 0.30;
/// Hard ceiling on L0-pinned entries. This is the *absolute* maximum the Phase 2
/// read-back token budget tolerates — config `max_pinned` may lower the
/// effective cap but can never raise it above this. `pin`/`list_pinned` honor
/// the effective cap (`UserProfileStore::max_pinned`).
pub const MAX_PINNED: i64 = 8;
/// Maximum stored `value` length (chars, truncated on a UTF-8 boundary).
pub const MAX_VALUE_LEN: usize = 512;
/// Maximum stored `key` length (chars, truncated on a UTF-8 boundary).
pub const MAX_KEY_LEN: usize = 128;
/// Sentinel `pin_rank` for non-pinned entries (sorts last).
pub const UNPINNED_RANK: i64 = 1_000_000;

/// Result of an [`UserProfileStore::upsert`] — whether a new row was created or
/// an existing `(category, key)` row was updated in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Inserted,
    Updated,
}

pub struct UserProfileStore {
    conn: Arc<Mutex<Connection>>,
    /// Effective confidence floor (operator-configurable, clamped to `0.0..=1.0`).
    min_confidence: f32,
    /// Effective L0 cap (operator-configurable, clamped to `0..=MAX_PINNED`).
    max_pinned: i64,
}

impl UserProfileStore {
    /// Open with the built-in default limits ([`MIN_INSERT_CONFIDENCE`],
    /// [`MAX_PINNED`]). Used by tests and any caller that doesn't thread config.
    pub async fn open(path: PathBuf) -> anyhow::Result<Self> {
        Self::open_with_limits(path, MIN_INSERT_CONFIDENCE, MAX_PINNED).await
    }

    /// Open at `path`, create the parent dir, set WAL pragmas, run migrations.
    ///
    /// `min_confidence` is clamped to `0.0..=1.0`; `max_pinned` is clamped to
    /// `0..=MAX_PINNED` so an operator can only make the L0 set *more*
    /// conservative — never large enough to blow the Phase 2 token budget.
    pub async fn open_with_limits(
        path: PathBuf,
        min_confidence: f32,
        max_pinned: i64,
    ) -> anyhow::Result<Self> {
        let min_confidence = min_confidence.clamp(0.0, 1.0);
        let max_pinned = max_pinned.clamp(0, MAX_PINNED);
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            let conn = Connection::open(&path)
                .with_context(|| format!("open user_profile.db at {}", path.display()))?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS profile_entries (
                    id           TEXT PRIMARY KEY,
                    category     TEXT NOT NULL,
                    key          TEXT NOT NULL,
                    value        TEXT NOT NULL,
                    confidence   REAL NOT NULL CHECK (confidence BETWEEN 0.0 AND 1.0),
                    source_kind  TEXT NOT NULL,
                    source_ref   TEXT,
                    pin_rank     INTEGER NOT NULL,
                    usage_count  INTEGER NOT NULL DEFAULT 0,
                    last_used    TEXT,
                    created_at   TEXT NOT NULL,
                    updated_at   TEXT NOT NULL,
                    status       TEXT NOT NULL DEFAULT 'active'
                                 CHECK (status IN ('active','archived')),
                    UNIQUE(category, key)
                );
                CREATE INDEX IF NOT EXISTS idx_profile_status_pin
                  ON profile_entries(status, pin_rank);
                CREATE TABLE IF NOT EXISTS profile_meta (
                    id      INTEGER PRIMARY KEY CHECK (id = 1),
                    version INTEGER NOT NULL DEFAULT 0
                );
                INSERT OR IGNORE INTO profile_meta (id, version) VALUES (1, 0);",
            )?;
            Ok(conn)
        })
        .await
        .context("spawn_blocking open user_profile")??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            min_confidence,
            max_pinned,
        })
    }

    /// The store's effective confidence floor. Promotion clamps to this so a
    /// floor-clamped pref can never be rejected by `upsert`.
    pub fn min_confidence(&self) -> f32 {
        self.min_confidence
    }

    /// The store's effective L0 cap.
    pub fn max_pinned(&self) -> i64 {
        self.max_pinned
    }

    /// Insert or update by `(category, key)`. Enforces [`MIN_INSERT_CONFIDENCE`]
    /// (rejects with `Err` — fail-closed), truncates `value`/`key` to caps on a
    /// char boundary, refreshes `updated_at`, and bumps `version` in the same
    /// transaction. On an existing `(category, key)` the value/confidence/source
    /// are refreshed but `pin_rank`/`usage_count`/`created_at` are preserved.
    pub async fn upsert(&self, entry: ProfileEntry) -> anyhow::Result<UpsertOutcome> {
        if entry.confidence < self.min_confidence {
            anyhow::bail!(
                "confidence {:.2} below floor {:.2}",
                entry.confidence,
                self.min_confidence
            );
        }
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<UpsertOutcome> {
            let key = truncate_chars(&entry.key, MAX_KEY_LEN);
            let value = truncate_chars(&entry.value, MAX_VALUE_LEN);
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let tx = guard.unchecked_transaction()?;

            // Does a row already exist for this (category, key)?
            let existing_id: Option<String> = tx
                .query_row(
                    "SELECT id FROM profile_entries WHERE category = ?1 AND key = ?2",
                    params![entry.category.as_str(), key],
                    |r| r.get(0),
                )
                .ok();

            let outcome = if let Some(id) = existing_id {
                tx.execute(
                    "UPDATE profile_entries
                     SET value = ?1, confidence = ?2, source_kind = ?3, source_ref = ?4,
                         status = ?5, updated_at = ?6
                     WHERE id = ?7",
                    params![
                        value,
                        entry.confidence,
                        entry.source.discriminant(),
                        entry.source.source_ref(),
                        entry.status.as_str(),
                        entry.updated_at.to_rfc3339(),
                        id,
                    ],
                )?;
                UpsertOutcome::Updated
            } else {
                tx.execute(
                    "INSERT INTO profile_entries
                     (id, category, key, value, confidence, source_kind, source_ref,
                      pin_rank, usage_count, last_used, created_at, updated_at, status)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        entry.id.to_string(),
                        entry.category.as_str(),
                        key,
                        value,
                        entry.confidence,
                        entry.source.discriminant(),
                        entry.source.source_ref(),
                        entry.pin_rank,
                        entry.usage_count,
                        entry.last_used.map(|d| d.to_rfc3339()),
                        entry.created_at.to_rfc3339(),
                        entry.updated_at.to_rfc3339(),
                        entry.status.as_str(),
                    ],
                )?;
                UpsertOutcome::Inserted
            };

            bump_version(&tx)?;
            tx.commit()?;
            Ok(outcome)
        })
        .await
        .context("spawn_blocking upsert profile entry")?
    }

    /// Count of active profile entries (cheaper than `list` for status display).
    pub async fn count_active(&self) -> anyhow::Result<usize> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let n: i64 = guard.query_row(
                "SELECT COUNT(*) FROM profile_entries WHERE status = 'active'",
                [],
                |r| r.get(0),
            )?;
            Ok(n as usize)
        })
        .await
        .context("spawn_blocking count_active profile entries")?
    }

    /// Active entries ordered by pin_rank ASC, updated_at DESC.
    pub async fn list(&self, limit: u32) -> anyhow::Result<Vec<ProfileEntry>> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ProfileEntry>> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let mut stmt = guard.prepare(
                "SELECT id, category, key, value, confidence, source_kind, source_ref,
                        pin_rank, usage_count, last_used, created_at, updated_at, status
                 FROM profile_entries
                 WHERE status = 'active'
                 ORDER BY pin_rank ASC, updated_at DESC
                 LIMIT ?1",
            )?;
            let mut rows = stmt.query(params![limit])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(row_to_entry(row)?);
            }
            Ok(out)
        })
        .await
        .context("spawn_blocking list profile entries")?
    }

    /// Up to the effective `max_pinned` active L0 entries (pin_rank <
    /// [`UNPINNED_RANK`]), ordered by pin_rank ASC. This is the L0 read-back
    /// source for Phase 2.
    pub async fn list_pinned(&self) -> anyhow::Result<Vec<ProfileEntry>> {
        let conn = Arc::clone(&self.conn);
        let max_pinned = self.max_pinned;
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ProfileEntry>> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let mut stmt = guard.prepare(
                "SELECT id, category, key, value, confidence, source_kind, source_ref,
                        pin_rank, usage_count, last_used, created_at, updated_at, status
                 FROM profile_entries
                 WHERE status = 'active' AND pin_rank < ?1
                 ORDER BY pin_rank ASC
                 LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![UNPINNED_RANK, max_pinned])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(row_to_entry(row)?);
            }
            Ok(out)
        })
        .await
        .context("spawn_blocking list_pinned profile entries")?
    }

    pub async fn get(&self, id: &str) -> anyhow::Result<Option<ProfileEntry>> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<ProfileEntry>> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let mut stmt = guard.prepare(
                "SELECT id, category, key, value, confidence, source_kind, source_ref,
                        pin_rank, usage_count, last_used, created_at, updated_at, status
                 FROM profile_entries WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            if let Some(row) = rows.next()? {
                Ok(Some(row_to_entry(row)?))
            } else {
                Ok(None)
            }
        })
        .await
        .context("spawn_blocking get profile entry")?
    }

    /// Apply a partial update; bumps `version` if any field was set. Returns
    /// true if the entry exists (i.e. the edit targeted a real row), false if
    /// no entry has that id. `value` is truncated to the cap; `confidence` is
    /// range-checked.
    ///
    /// The return value is keyed on *existence*, not on rows-changed: setting a
    /// field to its current value changes 0 rows in SQLite but the entry was
    /// still found, so this returns true (avoids a misleading "not found").
    pub async fn edit(&self, id: &str, patch: ProfilePatch) -> anyhow::Result<bool> {
        if let Some(c) = patch.confidence {
            if !(0.0..=1.0).contains(&c) {
                anyhow::bail!("confidence {c:.2} out of range 0.0..=1.0");
            }
        }
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let tx = guard.unchecked_transaction()?;
            let now = chrono::Utc::now().to_rfc3339();

            // Existence is the source of truth for the return value. COUNT(*)
            // always yields exactly one row, so `?` still surfaces real DB
            // errors (a genuine failure is not silently mapped to "not found").
            let exists: i64 = tx.query_row(
                "SELECT COUNT(*) FROM profile_entries WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )?;
            if exists == 0 {
                tx.commit()?;
                return Ok(false);
            }

            let mut any_field = false;
            if let Some(cat) = patch.category {
                tx.execute(
                    "UPDATE profile_entries SET category = ?1, updated_at = ?2 WHERE id = ?3",
                    params![cat.as_str(), now, id],
                )?;
                any_field = true;
            }
            if let Some(value) = patch.value {
                let value = truncate_chars(&value, MAX_VALUE_LEN);
                tx.execute(
                    "UPDATE profile_entries SET value = ?1, updated_at = ?2 WHERE id = ?3",
                    params![value, now, id],
                )?;
                any_field = true;
            }
            if let Some(confidence) = patch.confidence {
                tx.execute(
                    "UPDATE profile_entries SET confidence = ?1, updated_at = ?2 WHERE id = ?3",
                    params![confidence, now, id],
                )?;
                any_field = true;
            }
            if let Some(pin_rank) = patch.pin_rank {
                tx.execute(
                    "UPDATE profile_entries SET pin_rank = ?1, updated_at = ?2 WHERE id = ?3",
                    params![pin_rank, now, id],
                )?;
                any_field = true;
            }
            if let Some(status) = patch.status {
                tx.execute(
                    "UPDATE profile_entries SET status = ?1, updated_at = ?2 WHERE id = ?3",
                    params![status.as_str(), now, id],
                )?;
                any_field = true;
            }

            if any_field {
                bump_version(&tx)?;
            }
            tx.commit()?;
            Ok(true)
        })
        .await
        .context("spawn_blocking edit profile entry")?
    }

    /// Hard-delete (forget); bumps `version`. Returns true if a row was removed.
    pub async fn forget(&self, id: &str) -> anyhow::Result<bool> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let tx = guard.unchecked_transaction()?;
            let removed = tx.execute("DELETE FROM profile_entries WHERE id = ?1", params![id])?;
            if removed > 0 {
                bump_version(&tx)?;
            }
            tx.commit()?;
            Ok(removed > 0)
        })
        .await
        .context("spawn_blocking forget profile entry")?
    }

    /// Apply decay and archival updates from the hourly sweep in a **single
    /// transaction**, avoiding the O(N) sequential `spawn_blocking` round-trips
    /// that individual `edit()` calls would require.
    ///
    /// `decays` is a slice of `(id, new_pin_rank)` pairs for entries whose rank
    /// has changed. `archives` is a slice of ids to set to `status='archived'`.
    /// Both sets are applied together before `bump_version`; the lock is held
    /// for the duration of the transaction only.
    pub async fn apply_decay_batch(
        &self,
        decays: &[(String, i64)],
        archives: &[String],
    ) -> anyhow::Result<()> {
        if decays.is_empty() && archives.is_empty() {
            return Ok(());
        }
        let conn = Arc::clone(&self.conn);
        let decays = decays.to_vec();
        let archives = archives.to_vec();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let tx = guard.unchecked_transaction()?;
            let now = chrono::Utc::now().to_rfc3339();
            for (id, new_rank) in &decays {
                tx.execute(
                    "UPDATE profile_entries SET pin_rank = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![new_rank, now, id],
                )?;
            }
            for id in &archives {
                tx.execute(
                    "UPDATE profile_entries SET status = 'archived', updated_at = ?1 WHERE id = ?2",
                    rusqlite::params![now, id],
                )?;
            }
            if !decays.is_empty() || !archives.is_empty() {
                bump_version(&tx)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .context("spawn_blocking apply_decay_batch")?
    }

    /// Delete every profile entry (right-to-forget, Phase 6). Returns the number
    /// of rows removed; bumps `version` once if anything was removed.
    pub async fn clear_all(&self) -> anyhow::Result<usize> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let tx = guard.unchecked_transaction()?;
            let removed = tx.execute("DELETE FROM profile_entries", [])?;
            if removed > 0 {
                bump_version(&tx)?;
            }
            tx.commit()?;
            Ok(removed)
        })
        .await
        .context("spawn_blocking clear_all profile entries")?
    }

    /// Assign the next free `pin_rank` in `0..max_pinned` (Err if the L0 set is
    /// full); bumps `version`.
    pub async fn pin(&self, id: &str) -> anyhow::Result<()> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        let max_pinned = self.max_pinned;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let tx = guard.unchecked_transaction()?;

            // Already pinned? Idempotent no-op.
            let current: Option<i64> = tx
                .query_row(
                    "SELECT pin_rank FROM profile_entries WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .ok();
            match current {
                None => anyhow::bail!("profile entry not found: {id}"),
                Some(rank) if rank < UNPINNED_RANK => {
                    return Ok(()); // already pinned
                }
                _ => {}
            }

            // Find the lowest free rank in 0..max_pinned. Scan is
            // status-AGNOSTIC: an archived row may still hold a low pin_rank, so
            // counting it as taken keeps ranks globally unique and prevents two
            // entries colliding on the same rank if the archived one is later
            // re-activated (IMP-2).
            let mut taken = std::collections::HashSet::new();
            {
                let mut stmt =
                    tx.prepare("SELECT pin_rank FROM profile_entries WHERE pin_rank < ?1")?;
                let mut rows = stmt.query(params![UNPINNED_RANK])?;
                while let Some(row) = rows.next()? {
                    taken.insert(row.get::<_, i64>(0)?);
                }
            }
            let free = (0..max_pinned).find(|r| !taken.contains(r));
            let Some(rank) = free else {
                anyhow::bail!("pin set full (max {max_pinned})");
            };

            tx.execute(
                "UPDATE profile_entries SET pin_rank = ?1, updated_at = ?2 WHERE id = ?3",
                params![rank, chrono::Utc::now().to_rfc3339(), id],
            )?;
            bump_version(&tx)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .context("spawn_blocking pin profile entry")?
    }

    /// `usage_count += 1`, `last_used = now`; bumps `version`. Used by Phase 2's
    /// `mark_used` read-back feedback and Phase 5's active learning.
    pub async fn touch(&self, id: &str) -> anyhow::Result<()> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let tx = guard.unchecked_transaction()?;
            let now = chrono::Utc::now().to_rfc3339();
            // NOTE: `touch` intentionally does NOT bump the store version.
            // `usage_count`/`last_used` do not affect the rendered L0 block text,
            // so bumping here would invalidate the Phase 2 prompt-cache memo on every
            // task and defeat the entire caching goal. Version bumps are reserved for
            // mutations that change rendered output: upsert, edit, forget, clear_all, pin.
            let _updated = tx.execute(
                "UPDATE profile_entries
                 SET usage_count = usage_count + 1, last_used = ?1
                 WHERE id = ?2",
                params![now, id],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .context("spawn_blocking touch profile entry")?
    }

    /// Current monotonic version. Read by Phase 2 to gate prompt-cache re-render.
    pub async fn version(&self) -> anyhow::Result<u64> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
            let guard = conn
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            let v: i64 =
                guard.query_row("SELECT version FROM profile_meta WHERE id = 1", [], |r| {
                    r.get(0)
                })?;
            Ok(v as u64)
        })
        .await
        .context("spawn_blocking read profile version")?
    }
}

/// Bump the monotonic version counter. MUST be called inside the same
/// transaction as the mutation it accompanies so readers never observe a
/// changed row with a stale version.
fn bump_version(tx: &rusqlite::Transaction<'_>) -> anyhow::Result<()> {
    tx.execute(
        "UPDATE profile_meta SET version = version + 1 WHERE id = 1",
        [],
    )?;
    Ok(())
}

/// Truncate `s` to at most `max` chars on a UTF-8 boundary (never mid-codepoint).
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => s[..byte_idx].to_string(),
        None => s.to_string(),
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> anyhow::Result<ProfileEntry> {
    let id: String = row.get(0)?;
    let category: String = row.get(1)?;
    let source_kind: String = row.get(5)?;
    let source_ref: Option<String> = row.get(6)?;
    let last_used: Option<String> = row.get(9)?;
    let created_at: String = row.get(10)?;
    let updated_at: String = row.get(11)?;
    let status: String = row.get(12)?;
    Ok(ProfileEntry {
        id: id.parse().context("parse ProfileEntryID")?,
        category: ProfileCategory::from_str_lossy(&category),
        key: row.get(2)?,
        value: row.get(3)?,
        confidence: row.get(4)?,
        source: ProfileSource::from_parts(&source_kind, source_ref),
        pin_rank: row.get(7)?,
        usage_count: row.get(8)?,
        last_used: last_used
            .map(|s| {
                chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&chrono::Utc))
            })
            .transpose()?,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&chrono::Utc),
        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&chrono::Utc),
        status: ProfileEntryStatus::from_str_lossy(&status),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::ProfileEntryID;
    use tempfile::tempdir;

    fn entry(category: ProfileCategory, key: &str, value: &str, confidence: f32) -> ProfileEntry {
        let now = chrono::Utc::now();
        ProfileEntry {
            id: ProfileEntryID::new(),
            category,
            key: key.to_string(),
            value: value.to_string(),
            confidence,
            source: ProfileSource::Explicit,
            pin_rank: UNPINNED_RANK,
            usage_count: 0,
            last_used: None,
            created_at: now,
            updated_at: now,
            status: ProfileEntryStatus::Active,
        }
    }

    async fn store() -> UserProfileStore {
        let dir = tempdir().unwrap();
        let path = dir.path().join("user_profile.db");
        let s = UserProfileStore::open(path).await.unwrap();
        Box::leak(Box::new(dir));
        s
    }

    async fn store_with_limits(min_confidence: f32, max_pinned: i64) -> UserProfileStore {
        let dir = tempdir().unwrap();
        let path = dir.path().join("user_profile.db");
        let s = UserProfileStore::open_with_limits(path, min_confidence, max_pinned)
            .await
            .unwrap();
        Box::leak(Box::new(dir));
        s
    }

    #[tokio::test]
    async fn config_limits_are_honored_and_clamped() {
        // Operator floor of 0.6 rejects a 0.5 entry that the default 0.30 floor
        // would accept.
        let s = store_with_limits(0.6, 3).await;
        assert!(s
            .upsert(entry(ProfileCategory::Other, "k", "v", 0.5))
            .await
            .is_err());
        assert!(s
            .upsert(entry(ProfileCategory::Other, "k", "v", 0.7))
            .await
            .is_ok());

        // Effective L0 cap is the configured 3, not the MAX_PINNED ceiling.
        assert_eq!(s.max_pinned(), 3);
        let mut ids = Vec::new();
        for i in 0..4 {
            let e = entry(ProfileCategory::Other, &format!("p{i}"), "v", 0.9);
            ids.push(e.id.to_string());
            s.upsert(e).await.unwrap();
        }
        for id in ids.iter().take(3) {
            s.pin(id).await.unwrap();
        }
        assert!(
            s.pin(&ids[3]).await.is_err(),
            "4th pin must exceed configured cap of 3"
        );
        assert_eq!(s.list_pinned().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn config_limits_clamp_above_ceiling() {
        // max_pinned above the hard MAX_PINNED ceiling is clamped down; a
        // min_confidence above 1.0 is clamped to 1.0.
        let s = store_with_limits(2.0, 9999).await;
        assert_eq!(s.max_pinned(), MAX_PINNED);
        assert!((s.min_confidence() - 1.0).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn upsert_and_list() {
        let s = store().await;
        let e = entry(ProfileCategory::TechStack, "lang", "Rust", 0.9);
        let id = e.id;
        assert_eq!(s.upsert(e).await.unwrap(), UpsertOutcome::Inserted);
        let rows = s.list(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].category, ProfileCategory::TechStack);
        assert_eq!(rows[0].value, "Rust");
    }

    #[tokio::test]
    async fn upsert_rejects_below_confidence_floor() {
        let s = store().await;
        let e = entry(ProfileCategory::Other, "k", "v", 0.1);
        assert!(s.upsert(e).await.is_err());
        assert!(s.list(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_accepts_confidence_exactly_at_floor() {
        // The floor is inclusive: `confidence < MIN_INSERT_CONFIDENCE` rejects,
        // so a value exactly equal to the floor must be accepted.
        let s = store().await;
        let e = entry(ProfileCategory::Other, "k", "v", MIN_INSERT_CONFIDENCE);
        assert_eq!(s.upsert(e).await.unwrap(), UpsertOutcome::Inserted);
        assert_eq!(s.list(10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn edit_existing_row_with_noop_value_returns_true() {
        // Regression: editing an existing entry to its CURRENT value changes 0
        // rows in SQLite, but the entry exists — edit() must return true, not a
        // misleading "not found".
        let s = store().await;
        let e = entry(ProfileCategory::Other, "k", "same", 0.9);
        let id = e.id.to_string();
        s.upsert(e).await.unwrap();
        let found = s
            .edit(
                &id,
                ProfilePatch {
                    value: Some("same".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            found,
            "edit on an existing row must return true even on a no-op value"
        );
        // Editing a non-existent id returns false.
        let missing = s
            .edit(
                "00000000-0000-0000-0000-000000000000",
                ProfilePatch {
                    value: Some("x".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!missing);
    }

    #[tokio::test]
    async fn upsert_truncates_oversized_value() {
        let s = store().await;
        let big = "x".repeat(2000);
        let e = entry(ProfileCategory::Other, "k", &big, 0.9);
        s.upsert(e).await.unwrap();
        let rows = s.list(10).await.unwrap();
        assert!(rows[0].value.chars().count() <= MAX_VALUE_LEN);
        assert!(std::str::from_utf8(rows[0].value.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn upsert_same_category_key_updates_not_duplicates() {
        let s = store().await;
        s.upsert(entry(ProfileCategory::TechStack, "lang", "Rust", 0.9))
            .await
            .unwrap();
        let outcome = s
            .upsert(entry(ProfileCategory::TechStack, "lang", "Rust 2024", 0.95))
            .await
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Updated);
        let rows = s.list(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, "Rust 2024");
    }

    #[tokio::test]
    async fn version_bumps_on_render_affecting_mutations() {
        // `touch` intentionally does NOT bump version (usage stats don't affect the
        // rendered L0 block). All other mutations must bump it.
        let s = store().await;
        let v0 = s.version().await.unwrap();
        let e = entry(ProfileCategory::Other, "k", "v", 0.9);
        let id = e.id.to_string();
        s.upsert(e).await.unwrap();
        let v1 = s.version().await.unwrap();
        s.edit(
            &id,
            ProfilePatch {
                confidence: Some(0.8),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let v2 = s.version().await.unwrap();
        // `touch` does NOT bump the version — this is the key invariant that keeps the
        // L0 prompt-cache stable after render-path feedback (Phase 2 / Phase 5).
        s.touch(&id).await.unwrap();
        let v2b = s.version().await.unwrap();
        assert_eq!(v2, v2b, "touch must NOT bump the store version");
        s.forget(&id).await.unwrap();
        let v3 = s.version().await.unwrap();
        assert!(v1 > v0 && v2 > v1 && v3 > v2, "{v0} {v1} {v2} {v3}");
    }

    #[tokio::test]
    async fn touch_does_not_bump_version() {
        let s = store().await;
        let e = entry(ProfileCategory::Other, "k", "v", 0.9);
        let id = e.id.to_string();
        s.upsert(e).await.unwrap();
        let v_before = s.version().await.unwrap();
        s.touch(&id).await.unwrap();
        let v_after = s.version().await.unwrap();
        assert_eq!(
            v_before, v_after,
            "touch must not invalidate the L0 render cache"
        );
    }

    #[tokio::test]
    async fn forget_removes_entry_and_returns_true() {
        let s = store().await;
        let e = entry(ProfileCategory::Other, "k", "v", 0.9);
        let id = e.id.to_string();
        s.upsert(e).await.unwrap();
        assert!(s.forget(&id).await.unwrap());
        assert!(s.get(&id).await.unwrap().is_none());
        assert!(!s.forget(&id).await.unwrap());
    }

    #[tokio::test]
    async fn pin_enforces_max_pinned_cap() {
        let s = store().await;
        let mut ids = Vec::new();
        for i in 0..(MAX_PINNED + 1) {
            let e = entry(ProfileCategory::Other, &format!("k{i}"), "v", 0.9);
            ids.push(e.id.to_string());
            s.upsert(e).await.unwrap();
        }
        for id in ids.iter().take(MAX_PINNED as usize) {
            s.pin(id).await.unwrap();
        }
        assert!(s.pin(&ids[MAX_PINNED as usize]).await.is_err());
        assert_eq!(s.list_pinned().await.unwrap().len(), MAX_PINNED as usize);
    }

    #[tokio::test]
    async fn touch_increments_usage_and_sets_last_used() {
        let s = store().await;
        let e = entry(ProfileCategory::Other, "k", "v", 0.9);
        let id = e.id.to_string();
        s.upsert(e).await.unwrap();
        s.touch(&id).await.unwrap();
        let got = s.get(&id).await.unwrap().unwrap();
        assert_eq!(got.usage_count, 1);
        assert!(got.last_used.is_some());
    }

    #[tokio::test]
    async fn clear_all_removes_everything() {
        let s = store().await;
        for i in 0..3 {
            s.upsert(entry(ProfileCategory::Other, &format!("k{i}"), "v", 0.9))
                .await
                .unwrap();
        }
        assert_eq!(s.clear_all().await.unwrap(), 3);
        assert!(s.list(10).await.unwrap().is_empty());
    }

    /// Phase 7 M2 regression guard: `touch()` must NOT invalidate the L0
    /// prompt-cache memo. The Phase 2 version-gated renderer only re-renders the
    /// profile block when the store version changes; `touch` updates usage stats
    /// (usage_count, last_used) which do not affect rendered output, so it must
    /// leave the version counter unchanged. If this test breaks, the Anthropic
    /// cache breakpoint will be busted on every task that reads a profile entry.
    #[tokio::test]
    async fn touch_does_not_bust_cache() {
        let s = store().await;
        let e = entry(ProfileCategory::TechStack, "cache_key", "cached_value", 0.9);
        let id = e.id.to_string();
        s.upsert(e).await.unwrap();
        // Capture version after insert — this is what Phase 2 memoizes.
        let version_after_insert = s.version().await.unwrap();
        // Calling touch() stamps usage_count + last_used — stats, not rendered text.
        s.touch(&id).await.unwrap();
        let version_after_touch = s.version().await.unwrap();
        assert_eq!(
            version_after_insert, version_after_touch,
            "touch() must not bump the store version; doing so would bust the L0 prompt-cache \
             memo on every task and defeat Phase 2 caching goal"
        );
        // Sanity: the touch did update usage stats (non-regression on the touch itself).
        let got = s.get(&id).await.unwrap().unwrap();
        assert_eq!(got.usage_count, 1, "touch must increment usage_count");
        assert!(got.last_used.is_some(), "touch must stamp last_used");
    }

    /// Phase 7 regression guard: the confidence floor check is `<` (strict less-than),
    /// so a value exactly equal to `MIN_INSERT_CONFIDENCE` must be accepted.
    /// This pins the fix — if someone changes `<` to `<=` this breaks immediately.
    #[tokio::test]
    async fn upsert_on_floor_equals_accepts() {
        let s = store().await;
        // Exactly at the floor — must succeed (floor is inclusive).
        let at_floor = entry(
            ProfileCategory::Other,
            "floor_key",
            "floor_value",
            MIN_INSERT_CONFIDENCE,
        );
        let outcome = s.upsert(at_floor).await;
        assert!(
            outcome.is_ok(),
            "confidence exactly equal to MIN_INSERT_CONFIDENCE ({MIN_INSERT_CONFIDENCE}) \
             must be accepted; the check is `< floor`, not `<= floor`"
        );
        // Epsilon below the floor — must fail.
        let below_floor = entry(
            ProfileCategory::Other,
            "below_floor_key",
            "v",
            MIN_INSERT_CONFIDENCE - f32::EPSILON,
        );
        assert!(
            s.upsert(below_floor).await.is_err(),
            "confidence below MIN_INSERT_CONFIDENCE must be rejected"
        );
    }
}
