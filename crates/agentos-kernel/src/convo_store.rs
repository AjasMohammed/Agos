use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Speaker name for rows the human operator posts into a conversation. `@` is not
/// a legal agent name (`commands::agent::is_valid_agent_name`), so no participant
/// can produce a row that reads as the operator.
pub const USER_SPEAKER: &str = "@user";

/// SQLite-backed store for multi-agent conversations.
pub struct ConvoStore {
    conn: Mutex<Connection>,
    /// Convo ids with a live runner in this process. See [`ConvoStore::begin_run`].
    live_runs: Mutex<HashSet<String>>,
}

/// Why [`ConvoStore::claim_resume`] refused.
#[derive(Debug)]
pub enum ResumeError {
    NotFound,
    /// A runner is still live (or the convo is `running`) — resuming now would
    /// start a second loop over the same transcript.
    Busy,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for ResumeError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

/// Held by a running conversation loop; releases the convo id on drop, including
/// when the runner future is dropped mid-turn or panics.
pub struct RunGuard {
    store: Arc<ConvoStore>,
    id: String,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // Every normal exit has already written a terminal status. Still
        // `running` here means a panic or abort — settle it, or Continue would
        // refuse a conversation nothing is running.
        // ponytail: one sync SQLite write on the dropping thread; it only
        // matters on the abnormal path.
        {
            let conn = self.store.conn.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = conn.execute(
                "UPDATE agent_convos SET status = 'error' WHERE id = ?1 AND status = 'running'",
                params![self.id],
            ) {
                tracing::error!(convo_id = %self.id, error = %e, "Failed to settle abandoned convo run");
            }
        }
        self.store
            .live_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// Drop `<user_data>` framing a model mirrored back from the transcript it was
/// shown. Orchestrators wrap prior turns in those tags for injection safety and
/// some models copy the wrapper into their own reply; left in place the tags
/// render verbatim in the UI and re-enter the next turn's prompt, teaching every
/// later turn to copy them too. Applied by [`ConvoStore::add_turn`], so every
/// persist path is covered — call it directly only for text used before the
/// turn is stored (a stream event, an in-memory transcript).
pub fn strip_user_data_tags(s: &str) -> String {
    // ASCII-only case folding: `to_lowercase` can change byte length (e.g. `İ`),
    // which would desync the match offsets from the original string's bytes.
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(rel) = lower[search..].find("user_data>") {
        let end = search + rel + "user_data>".len();
        // Only the exact tags — `<user_data>` / `</user_data>` — are framing.
        let start = if lower[..search + rel].ends_with("</") {
            search + rel - 2
        } else if lower[..search + rel].ends_with('<') {
            search + rel - 1
        } else {
            search = end;
            continue;
        };
        out.push_str(&s[last..start]);
        last = end;
        search = end;
    }
    out.push_str(&s[last..]);
    out.trim().to_string()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentConvo {
    pub id: String,
    pub topic: String,
    /// Ordered list of agent names (participants).
    pub participants: Vec<String>,
    pub max_turns: u32,
    /// "running" | "complete" | "stopped" | "error"
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    /// How the thread was born: `"operator"` (UI/API) or `"dm"` (agent-message).
    /// Rows written before the column existed read `"operator"`.
    pub kind: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConvoTurn {
    pub id: i64,
    pub turn_number: u32,
    pub agent_name: String,
    pub content: String,
    pub tool_call_count: u32,
    pub created_at: String,
}

impl ConvoStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS agent_convos (
                 id              TEXT PRIMARY KEY,
                 topic           TEXT NOT NULL,
                 participants    TEXT NOT NULL,
                 max_turns       INTEGER NOT NULL DEFAULT 10,
                 status          TEXT NOT NULL DEFAULT 'running',
                 created_at      TEXT NOT NULL,
                 updated_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_convos_updated
                 ON agent_convos(updated_at DESC);
             CREATE TABLE IF NOT EXISTS convo_turns (
                 id              INTEGER PRIMARY KEY AUTOINCREMENT,
                 convo_id        TEXT NOT NULL REFERENCES agent_convos(id) ON DELETE CASCADE,
                 turn_number     INTEGER NOT NULL,
                 agent_name      TEXT NOT NULL,
                 content         TEXT NOT NULL,
                 tool_call_count INTEGER NOT NULL DEFAULT 0,
                 created_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_convo_turns_convo
                 ON convo_turns(convo_id, turn_number);",
        )?;

        // Additive migration: `kind` marks how the thread was born ('operator'
        // from the UI/API, 'dm' from an agent-message), `dm_key` is the
        // canonical pair key that makes an agent-to-agent thread continuous,
        // `dm_expires_at` is that session's wall-clock deadline. Probe-then-
        // ALTER rather than a version table: the columns are additive and
        // default-safe.
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(agent_convos)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        if !columns.iter().any(|c| c == "kind") {
            conn.execute_batch(
                "ALTER TABLE agent_convos ADD COLUMN kind TEXT NOT NULL DEFAULT 'operator';",
            )?;
        }
        if !columns.iter().any(|c| c == "dm_key") {
            conn.execute_batch("ALTER TABLE agent_convos ADD COLUMN dm_key TEXT;")?;
        }
        if !columns.iter().any(|c| c == "dm_expires_at") {
            conn.execute_batch("ALTER TABLE agent_convos ADD COLUMN dm_expires_at TEXT;")?;
        }
        // NOT unique: a pair accumulates one row per session. The lookup wants
        // the newest row for a key, so index the key with the ordering column.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_convos_dm_key
                 ON agent_convos(dm_key, updated_at DESC) WHERE dm_key IS NOT NULL;",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
            live_runs: Mutex::new(HashSet::new()),
        })
    }

    /// Settle conversations orphaned by a previous process.
    ///
    /// A convo only advances while its in-process runner task is alive, and that
    /// task cannot outlive the kernel — so anything still `running` belongs to a
    /// process that is gone, and leaving it makes the panel show a live-looking
    /// conversation nothing will ever move again (seen 2026-09-09: convo
    /// `cba164ef` sat `running` with zero turns across a restart).
    ///
    /// NOT called from [`Self::open`], and that placement is load-bearing: this
    /// cannot tell an orphan from another *live* process's in-flight convo, and
    /// a second `agentos start` boots another `Kernel` against the same data dir. A
    /// reconcile at open would wipe every running conversation before that second
    /// process failed its single-instance check and exited. Call it only once the
    /// bus socket bind has proved no other kernel is live.
    pub fn reconcile_orphaned(&self) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let orphaned = conn.execute(
            "UPDATE agent_convos SET status = 'error' WHERE status = 'running'",
            [],
        )?;
        if orphaned > 0 {
            tracing::warn!(
                count = orphaned,
                "Marked orphaned running conversations as error"
            );
        }
        Ok(orphaned)
    }

    pub fn create_convo(
        &self,
        topic: &str,
        participants: &[String],
        max_turns: u32,
    ) -> Result<String, rusqlite::Error> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let participants_json = serde_json::to_string(participants)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO agent_convos (id, topic, participants, max_turns, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?5)",
            params![id, topic, participants_json, max_turns, now],
        )?;
        Ok(id)
    }

    pub fn get_convo(&self, id: &str) -> Result<Option<AgentConvo>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, topic, participants, max_turns, status, created_at, updated_at, kind
             FROM agent_convos WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let participants_json: String = row.get(2)?;
            let participants: Vec<String> = match serde_json::from_str(&participants_json) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, "Corrupt participants JSON in convo row — returning empty list");
                    Vec::new()
                }
            };
            Ok(Some(AgentConvo {
                id: row.get(0)?,
                topic: row.get(1)?,
                participants,
                max_turns: row.get::<_, i64>(3)? as u32,
                status: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                kind: row.get(7)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Most-recent-first, capped at 100. `kind` filters to `"dm"` or
    /// `"operator"`; `None` returns every conversation.
    pub fn list_convos(&self, kind: Option<&str>) -> Result<Vec<AgentConvo>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, topic, participants, max_turns, status, created_at, updated_at, kind
             FROM agent_convos
             WHERE ?1 IS NULL OR kind = ?1
             ORDER BY updated_at DESC
             LIMIT 100",
        )?;
        let rows = stmt.query_map(params![kind], |row| {
            let participants_json: String = row.get(2)?;
            let participants: Vec<String> = match serde_json::from_str(&participants_json) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, "Corrupt participants JSON in convo row — returning empty list");
                    Vec::new()
                }
            };
            Ok(AgentConvo {
                id: row.get(0)?,
                topic: row.get(1)?,
                participants,
                max_turns: row.get::<_, i64>(3)? as u32,
                status: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                kind: row.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// Append a row and return its turn number. The number is assigned here
    /// (`MAX + 1` under the connection lock) rather than by the caller, so an
    /// operator message posted while an agent's turn is in flight cannot collide
    /// with that turn's row.
    pub fn add_turn(
        &self,
        convo_id: &str,
        agent_name: &str,
        content: &str,
        tool_call_count: u32,
    ) -> Result<u32, rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let content = strip_user_data_tags(content);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        let turn_number: i64 = tx.query_row(
            "SELECT COALESCE(MAX(turn_number), 0) + 1 FROM convo_turns WHERE convo_id = ?1",
            params![convo_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO convo_turns (convo_id, turn_number, agent_name, content, tool_call_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![convo_id, turn_number, agent_name, content, tool_call_count, now],
        )?;
        tx.execute(
            "UPDATE agent_convos SET updated_at = ?1 WHERE id = ?2",
            params![now, convo_id],
        )?;
        tx.commit()?;
        Ok(turn_number as u32)
    }

    /// Canonical key for the one DM thread shared by a pair of agents,
    /// order-independent.
    ///
    /// `|` is safe as a separator: `commands::agent::is_valid_agent_name` allows
    /// only alphanumerics, `-`, `_` and `.`, so no name can contain it or forge
    /// a key. Case is preserved — agent names match case-sensitively everywhere
    /// else.
    pub fn dm_key(a: &str, b: &str) -> String {
        let mut names = [a, b];
        names.sort_unstable();
        format!("{}|{}", names[0], names[1])
    }

    /// True when this process has a live runner for `convo_id`.
    ///
    /// `claim_resume` refuses both for a live runner and for a row merely
    /// stuck at `running`; callers that must tell those apart ask here.
    pub fn is_live(&self, convo_id: &str) -> bool {
        self.live_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(convo_id)
    }

    /// The pair's current DM session, opened on demand.
    ///
    /// Returns `(convo_id, created)`; `created` lets the caller skip
    /// `claim_resume`, since a fresh row is already `running`.
    ///
    /// Reuse rules, in order:
    ///  1. a session this process is actively running (`is_live`) — liveness
    ///     beats expiry, or a second thread would open under a running loop;
    ///  2. the newest session whose `dm_expires_at` is still in the future and
    ///     which has not been closed;
    ///  3. otherwise a new session.
    ///
    /// The SELECT and the INSERT run in one transaction on the single
    /// `Mutex<Connection>`, so two simultaneous first messages cannot open two
    /// sessions in-process.
    ///
    /// `dm_expires_at` is written once, here. It is never moved by a turn —
    /// only an operator extension moves it (`extend_dm`).
    pub fn find_or_create_dm(
        &self,
        a: &str,
        b: &str,
        max_turns: u32,
        ttl_secs: u64,
    ) -> Result<(String, bool), rusqlite::Error> {
        let key = Self::dm_key(a, b);
        let now = chrono::Utc::now();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;

        let newest: Option<(String, Option<String>, String)> = tx
            .query_row(
                "SELECT id, dm_expires_at, status FROM agent_convos
                 WHERE dm_key = ?1 ORDER BY updated_at DESC LIMIT 1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        if let Some((id, expires_at, status)) = newest {
            let live = self
                .live_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&id);
            let unexpired = status == "running"
                && expires_at
                    .as_deref()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| t > now);
            // A finished session inside its lifetime is still the pair's
            // current session: `claim_resume` reopens it for more turns. Only
            // the clock, or an explicit close, ends a session.
            let reusable_complete = status == "complete"
                && expires_at
                    .as_deref()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| t > now);
            if live || unexpired || reusable_complete {
                tx.commit()?;
                return Ok((id, false));
            }
        }

        let id = Uuid::new_v4().to_string();
        let mut participants = [a, b];
        participants.sort_unstable();
        let participants_json = serde_json::to_string(&participants)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let expires_at = (now + chrono::Duration::seconds(ttl_secs as i64)).to_rfc3339();
        tx.execute(
            "INSERT INTO agent_convos
                 (id, topic, participants, max_turns, status, created_at, updated_at,
                  kind, dm_key, dm_expires_at)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?5, 'dm', ?6, ?7)",
            params![
                id,
                format!(
                    "Direct messages between {} and {}",
                    participants[0], participants[1]
                ),
                participants_json,
                max_turns,
                now.to_rfc3339(),
                key,
                expires_at
            ],
        )?;
        tx.commit()?;
        Ok((id, true))
    }

    /// One line about this pair's earlier sessions, for the prompt header of a
    /// DM session. `None` for operator convos and for a pair's first session.
    ///
    /// Read by `convo_runner::run_convo` itself, so no call site gains a
    /// parameter. Stable for the whole session, which keeps the prompt header
    /// prefix-cacheable.
    pub fn dm_history_note(&self, convo_id: &str) -> Result<Option<String>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let key: Option<String> = conn
            .query_row(
                "SELECT dm_key FROM agent_convos WHERE id = ?1",
                params![convo_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(key) = key else { return Ok(None) };

        let (count, last): (i64, Option<String>) = conn.query_row(
            "SELECT COUNT(*), MAX(updated_at) FROM agent_convos
             WHERE dm_key = ?1 AND id != ?2",
            params![key, convo_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if count == 0 {
            return Ok(None);
        }
        Ok(Some(format!(
            "Earlier sessions with this agent: {count}{}. \
             Use memory-search if you need what was said.",
            last.map(|t| format!(" (the last ended {t})"))
                .unwrap_or_default()
        )))
    }

    /// DM sessions whose clock has run out while they are still running.
    ///
    /// Only `running` rows matter: a `complete` / `stopped` / `error` session is
    /// already over, and its lapsed deadline is enforced passively by
    /// `find_or_create_dm`, which will not reuse it.
    pub fn expired_running_dm_sessions(
        &self,
    ) -> Result<Vec<(String, Vec<String>)>, rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, participants FROM agent_convos
             WHERE kind = 'dm' AND status = 'running'
               AND dm_expires_at IS NOT NULL AND dm_expires_at <= ?1",
        )?;
        let rows = stmt.query_map(params![now], |row| {
            let participants_json: String = row.get(1)?;
            let participants: Vec<String> =
                serde_json::from_str(&participants_json).unwrap_or_default();
            Ok((row.get::<_, String>(0)?, participants))
        })?;
        rows.collect()
    }

    /// This convo's DM deadline, if it has one. Operator convos return `None`.
    ///
    /// Read as its own query rather than added to [`AgentConvo`]: that struct is
    /// serialized onto the REST surface, and the deadline is only wanted by the
    /// runner when it sizes the shared workspace's lifetime.
    pub fn dm_deadline(
        &self,
        convo_id: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let raw: Option<String> = conn
            .query_row(
                "SELECT dm_expires_at FROM agent_convos WHERE id = ?1",
                params![convo_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(raw.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|d| d.with_timezone(&chrono::Utc))
        }))
    }

    /// Push a DM session's deadline out by `secs` from now.
    ///
    /// Used to grant an operator's extension, and to hold a session open while
    /// the extension question is pending so the next sweep cannot ask twice.
    pub fn extend_dm(&self, convo_id: &str, secs: u64) -> Result<(), rusqlite::Error> {
        let until = (chrono::Utc::now() + chrono::Duration::seconds(secs as i64)).to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE agent_convos SET dm_expires_at = ?1 WHERE id = ?2 AND dm_key IS NOT NULL",
            params![until, convo_id],
        )?;
        Ok(())
    }

    /// Register a live runner for `convo_id`. `None` if one is already running in
    /// this process.
    pub fn begin_run(self: &Arc<Self>, convo_id: &str) -> Option<RunGuard> {
        let mut runs = self.live_runs.lock().unwrap_or_else(|e| e.into_inner());
        runs.insert(convo_id.to_string()).then(|| RunGuard {
            store: Arc::clone(self),
            id: convo_id.to_string(),
        })
    }

    /// Reopen a finished conversation for `extra_turns` more agent turns: sets
    /// `running` and `max_turns = agent turns so far + extra_turns`, returning the
    /// new ceiling. Refuses while a runner is live — a stopped convo's runner
    /// finishes its in-flight turn first, and resuming under it would put two
    /// loops on one transcript.
    pub fn claim_resume(&self, convo_id: &str, extra_turns: u32) -> Result<u32, ResumeError> {
        // Checked and released before touching SQLite. Safe: a new runner only
        // starts after a create or claim wrote `running`, which the conditional
        // UPDATE below refuses.
        if self
            .live_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(convo_id)
        {
            return Err(ResumeError::Busy);
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let changed = conn.execute(
            "UPDATE agent_convos
             SET status = 'running',
                 max_turns = ?1 + (SELECT COUNT(*) FROM convo_turns
                                   WHERE convo_id = ?2 AND agent_name != ?3),
                 updated_at = ?4
             WHERE id = ?2 AND status != 'running'",
            params![
                extra_turns,
                convo_id,
                USER_SPEAKER,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        match conn.query_row(
            "SELECT max_turns FROM agent_convos WHERE id = ?1",
            params![convo_id],
            |r| r.get::<_, i64>(0),
        ) {
            Ok(ceiling) if changed == 1 => Ok(ceiling as u32),
            Ok(_) => Err(ResumeError::Busy),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(ResumeError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    /// Raise a conversation's turn ceiling (the runner grants a round to an
    /// operator message that arrives as the budget runs out).
    pub fn set_max_turns(&self, convo_id: &str, max_turns: u32) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE agent_convos SET max_turns = ?1 WHERE id = ?2",
            params![max_turns, convo_id],
        )?;
        Ok(())
    }

    pub fn get_turns(&self, convo_id: &str) -> Result<Vec<ConvoTurn>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, turn_number, agent_name, content, tool_call_count, created_at
             FROM convo_turns
             WHERE convo_id = ?1
             ORDER BY turn_number ASC",
        )?;
        let rows = stmt.query_map(params![convo_id], |row| {
            Ok(ConvoTurn {
                id: row.get(0)?,
                turn_number: row.get::<_, i64>(1)? as u32,
                agent_name: row.get(2)?,
                content: row.get(3)?,
                tool_call_count: row.get::<_, i64>(4)? as u32,
                created_at: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    pub fn set_status(&self, convo_id: &str, status: &str) -> Result<(), rusqlite::Error> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let changed = conn.execute(
            "UPDATE agent_convos SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, convo_id],
        )?;
        // Surface a missing convo to callers that care (e.g. the user-facing stop
        // handler returns 404); internal status writes ignore the result.
        if changed == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::strip_user_data_tags;

    #[test]
    fn strips_mirrored_user_data_framing() {
        assert_eq!(
            strip_user_data_tags("<user_data>hello</user_data>"),
            "hello"
        );
        assert_eq!(
            strip_user_data_tags("a <USER_DATA>b</User_Data> c <user_data>d</user_data>"),
            "a b c d"
        );
        assert_eq!(strip_user_data_tags("  plain reply\n"), "plain reply");
        // Non-ASCII must not desync the match offsets from the byte indices.
        assert_eq!(
            strip_user_data_tags("İstanbul <user_data>hi</user_data> \u{212A}elvin"),
            "İstanbul hi \u{212A}elvin"
        );
        // Only the exact tags are framing; a bare mention survives.
        assert_eq!(
            strip_user_data_tags("the user_data> marker and <user_datax> stay"),
            "the user_data> marker and <user_datax> stay"
        );
        // The escaped form the wrapper emits is content, not framing.
        assert_eq!(
            strip_user_data_tags("&lt;user_data&gt;x&lt;/user_data&gt;"),
            "&lt;user_data&gt;x&lt;/user_data&gt;"
        );
    }
}

#[cfg(test)]
mod reconcile_tests {
    use super::*;

    /// A convo only advances while its runner task is alive, and that task dies
    /// with the kernel. Anything left `running` across a restart is orphaned and
    /// must not keep reading as live — see the 2026-09-09 incident where
    /// `cba164ef` sat `running` with zero turns indefinitely.
    #[test]
    fn open_reconciles_orphaned_running_convos() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("convos.db");

        let id = {
            let store = ConvoStore::open(&path).expect("open");
            store
                .create_convo("topic", &["A".to_string(), "B".to_string()], 4)
                .expect("create")
        };

        // Reopen == kernel restart; the kernel calls this once the bus bind has
        // proved it is the only live instance.
        let store = ConvoStore::open(&path).expect("reopen");
        assert_eq!(store.reconcile_orphaned().expect("reconcile"), 1);
        let convo = store.get_convo(&id).expect("get").expect("row exists");
        assert_eq!(
            convo.status, "error",
            "an orphaned running convo must be settled at open"
        );
    }

    #[test]
    fn open_leaves_terminal_status_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("convos.db");

        let (done, stopped) = {
            let store = ConvoStore::open(&path).expect("open");
            let a = store
                .create_convo("a", &["A".to_string(), "B".to_string()], 4)
                .expect("create");
            let b = store
                .create_convo("b", &["A".to_string(), "B".to_string()], 4)
                .expect("create");
            store.set_status(&a, "complete").expect("set complete");
            store.set_status(&b, "stopped").expect("set stopped");
            (a, b)
        };

        let store = ConvoStore::open(&path).expect("reopen");
        assert_eq!(store.reconcile_orphaned().expect("reconcile"), 0);
        assert_eq!(store.get_convo(&done).unwrap().unwrap().status, "complete");
        assert_eq!(
            store.get_convo(&stopped).unwrap().unwrap().status,
            "stopped"
        );
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    #[test]
    fn claim_resume_extends_budget_and_refuses_live_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(ConvoStore::open(&dir.path().join("c.db")).expect("open"));
        let id = store
            .create_convo("t", &["A".to_string(), "B".to_string()], 2)
            .expect("create");

        assert_eq!(store.add_turn(&id, "A", "one", 0).unwrap(), 1);
        assert_eq!(store.add_turn(&id, USER_SPEAKER, "steer", 0).unwrap(), 2);
        assert_eq!(store.add_turn(&id, "B", "two", 0).unwrap(), 3);

        // Still `running` from creation: a second loop must not start.
        assert!(matches!(store.claim_resume(&id, 4), Err(ResumeError::Busy)));
        store.set_status(&id, "complete").unwrap();

        // A live runner (e.g. a stopped run finishing its turn) also blocks.
        let guard = store.begin_run(&id).expect("first runner");
        assert!(store.begin_run(&id).is_none());
        assert!(matches!(store.claim_resume(&id, 4), Err(ResumeError::Busy)));
        drop(guard);
        assert_eq!(store.get_convo(&id).unwrap().unwrap().status, "complete");

        // Operator rows don't count: 2 agent turns + 4.
        assert_eq!(store.claim_resume(&id, 4).unwrap(), 6);
        let convo = store.get_convo(&id).unwrap().unwrap();
        assert_eq!((convo.status.as_str(), convo.max_turns), ("running", 6));

        assert!(matches!(
            store.claim_resume("nope", 4),
            Err(ResumeError::NotFound)
        ));
    }

    /// A runner that dies without writing a terminal status (panic, abort) must
    /// not leave the convo `running` — Continue would refuse it forever.
    #[test]
    fn dropped_runner_settles_running_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(ConvoStore::open(&dir.path().join("c.db")).expect("open"));
        let id = store
            .create_convo("t", &["A".to_string(), "B".to_string()], 2)
            .expect("create");
        drop(store.begin_run(&id).expect("runner"));
        assert_eq!(store.get_convo(&id).unwrap().unwrap().status, "error");
        assert_eq!(store.claim_resume(&id, 2).unwrap(), 2);
    }

    // ── DM sessions ──────────────────────────────────────────────────────

    fn dm_store() -> (tempfile::TempDir, Arc<ConvoStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(ConvoStore::open(&dir.path().join("c.db")).expect("open"));
        (dir, store)
    }

    #[test]
    fn dm_key_is_order_independent() {
        assert_eq!(
            ConvoStore::dm_key("OSS", "Sandae"),
            ConvoStore::dm_key("Sandae", "OSS")
        );
        assert_eq!(ConvoStore::dm_key("Sandae", "OSS"), "OSS|Sandae");
    }

    #[test]
    fn find_or_create_dm_reuses_an_unexpired_session() {
        let (_d, store) = dm_store();
        let (first, created) = store.find_or_create_dm("Sandae", "OSS", 6, 3600).unwrap();
        assert!(created);
        // Swapped argument order must land on the same session.
        let (second, created_again) = store.find_or_create_dm("OSS", "Sandae", 6, 3600).unwrap();
        assert_eq!(first, second);
        assert!(!created_again);

        let convo = store.get_convo(&first).unwrap().unwrap();
        assert_eq!(convo.kind, "dm");
        assert_eq!(
            convo.participants,
            vec!["OSS".to_string(), "Sandae".to_string()]
        );
    }

    #[test]
    fn an_expired_session_opens_a_new_one() {
        let (_d, store) = dm_store();
        let (first, _) = store.find_or_create_dm("A", "B", 6, 0).unwrap();
        let (second, created) = store.find_or_create_dm("A", "B", 6, 0).unwrap();
        assert_ne!(first, second);
        assert!(created);
        assert_eq!(store.list_convos(Some("dm")).unwrap().len(), 2);
    }

    /// Liveness beats expiry: a second thread must not open under a running loop.
    #[test]
    fn a_live_session_is_reused_even_when_expired() {
        let (_d, store) = dm_store();
        let (first, _) = store.find_or_create_dm("A", "B", 6, 0).unwrap();
        let _guard = store.begin_run(&first).expect("runner");
        let (second, created) = store.find_or_create_dm("A", "B", 6, 0).unwrap();
        assert_eq!(first, second);
        assert!(!created);
    }

    /// The deadline is a fixed lifetime — only an operator extension moves it.
    #[test]
    fn add_turn_leaves_the_deadline_alone() {
        let (_d, store) = dm_store();
        let (id, _) = store.find_or_create_dm("A", "B", 6, 3600).unwrap();
        let before = dm_expires_at(&store, &id);
        assert!(before.is_some());
        for _ in 0..3 {
            store.add_turn(&id, "A", "hello", 0).unwrap();
        }
        assert_eq!(dm_expires_at(&store, &id), before);

        store.extend_dm(&id, 7200).unwrap();
        assert_ne!(dm_expires_at(&store, &id), before);

        // An operator convo has no dm_key, so it never gains a deadline.
        let op = store
            .create_convo("t", &["A".into(), "B".into()], 2)
            .unwrap();
        store.extend_dm(&op, 3600).unwrap();
        assert_eq!(dm_expires_at(&store, &op), None);
    }

    fn dm_expires_at(store: &ConvoStore, id: &str) -> Option<String> {
        let conn = store.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT dm_expires_at FROM agent_convos WHERE id = ?1",
            params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .unwrap()
    }

    #[test]
    fn dm_and_operator_threads_coexist_and_filter() {
        let (_d, store) = dm_store();
        let op = store
            .create_convo("debate", &["A".into(), "B".into()], 4)
            .unwrap();
        let (dm, _) = store.find_or_create_dm("A", "B", 6, 3600).unwrap();

        assert_eq!(store.get_convo(&op).unwrap().unwrap().kind, "operator");
        assert_eq!(store.list_convos(None).unwrap().len(), 2);

        let dms = store.list_convos(Some("dm")).unwrap();
        assert_eq!(dms.len(), 1);
        assert_eq!(dms[0].id, dm);
        let ops = store.list_convos(Some("operator")).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].id, op);
    }

    #[test]
    fn dm_history_note_counts_earlier_sessions() {
        let (_d, store) = dm_store();
        let op = store
            .create_convo("t", &["A".into(), "B".into()], 2)
            .unwrap();
        assert_eq!(store.dm_history_note(&op).unwrap(), None);

        let (first, _) = store.find_or_create_dm("A", "B", 6, 0).unwrap();
        // A pair's first session has nothing to point back to.
        assert_eq!(store.dm_history_note(&first).unwrap(), None);

        let (second, _) = store.find_or_create_dm("A", "B", 6, 0).unwrap();
        let note = store.dm_history_note(&second).unwrap().expect("note");
        assert!(
            note.contains("Earlier sessions with this agent: 1"),
            "{note}"
        );

        let (third, _) = store.find_or_create_dm("A", "B", 6, 0).unwrap();
        let note = store.dm_history_note(&third).unwrap().expect("note");
        assert!(
            note.contains("Earlier sessions with this agent: 2"),
            "{note}"
        );
        assert!(note.contains("memory-search"), "{note}");
    }

    #[test]
    fn expired_running_sessions_are_swept_but_finished_ones_are_not() {
        let (_d, store) = dm_store();
        let (live, _) = store.find_or_create_dm("A", "B", 6, 3600).unwrap();
        let (lapsed, _) = store.find_or_create_dm("C", "D", 6, 0).unwrap();
        let (done, _) = store.find_or_create_dm("E", "F", 6, 0).unwrap();
        store.set_status(&done, "complete").unwrap();
        // Operator convos have no deadline and must never be swept.
        store
            .create_convo("t", &["A".into(), "B".into()], 2)
            .unwrap();

        let expired = store.expired_running_dm_sessions().unwrap();
        let ids: Vec<_> = expired.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec![lapsed.as_str()]);
        assert!(!ids.contains(&live.as_str()));

        let (_, participants) = &expired[0];
        assert_eq!(participants, &vec!["C".to_string(), "D".to_string()]);

        // Extending it takes it back out of the sweep.
        store.extend_dm(&lapsed, 3600).unwrap();
        assert!(store.expired_running_dm_sessions().unwrap().is_empty());
    }

    #[test]
    fn is_live_tracks_begin_run() {
        let (_d, store) = dm_store();
        let (id, _) = store.find_or_create_dm("A", "B", 6, 3600).unwrap();
        assert!(!store.is_live(&id));
        {
            let _guard = store.begin_run(&id).expect("runner");
            assert!(store.is_live(&id));
        }
        assert!(!store.is_live(&id));
    }

    /// Re-opening an existing database must not lose rows or re-run the ALTERs.
    #[test]
    fn migration_is_idempotent_on_an_existing_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("c.db");
        let id = {
            let store = ConvoStore::open(&path).expect("open");
            store
                .create_convo("t", &["A".to_string(), "B".to_string()], 2)
                .expect("create")
        };
        let store = ConvoStore::open(&path).expect("reopen");
        let convo = store.get_convo(&id).unwrap().unwrap();
        assert_eq!(convo.kind, "operator");
        assert_eq!(convo.topic, "t");
    }
}
