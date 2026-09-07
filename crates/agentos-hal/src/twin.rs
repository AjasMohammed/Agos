use agentos_types::{AgentID, AgentOSError};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::RwLock;

/// A device twin tracks the desired and reported state of an IoT device.
///
/// The desired state is what the agent wants (set by `set_desired`).
/// The reported state is what the physical sensor reports (set by `update_reported`).
/// The twin is "reconciled" when desired == reported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceTwin {
    pub device_id: String,
    pub device_type: String,
    pub desired_state: Value,
    pub reported_state: Value,
    pub desired_at: Option<DateTime<Utc>>,
    pub reported_at: Option<DateTime<Utc>>,
    pub desired_by: Option<AgentID>,
    pub reconciled: bool,
}

/// SQLite-backed registry for device twins.
pub struct TwinRegistry {
    db: tokio::sync::Mutex<Connection>,
    twins: RwLock<HashMap<String, DeviceTwin>>,
}

impl TwinRegistry {
    /// Open or create the twin registry database.
    pub fn new(db_path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path)
            .map_err(|e| AgentOSError::HalError(format!("Twin DB open failed: {e}")))?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;

             CREATE TABLE IF NOT EXISTS device_twins (
                 device_id TEXT PRIMARY KEY,
                 device_type TEXT NOT NULL,
                 desired_state TEXT NOT NULL DEFAULT '{}',
                 reported_state TEXT NOT NULL DEFAULT '{}',
                 desired_at TEXT,
                 reported_at TEXT,
                 desired_by TEXT,
                 reconciled INTEGER NOT NULL DEFAULT 1
             );",
        )
        .map_err(|e| AgentOSError::HalError(format!("Twin schema init failed: {e}")))?;

        // Load existing twins into memory
        let mut twins = HashMap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT device_id, device_type, desired_state, reported_state,
                            desired_at, reported_at, desired_by, reconciled
                     FROM device_twins",
                )
                .map_err(|e| AgentOSError::HalError(format!("Twin load query failed: {e}")))?;

            let rows = stmt
                .query_map([], |row| {
                    let device_id: String = row.get(0)?;
                    let device_type: String = row.get(1)?;
                    let desired_str: String = row.get(2)?;
                    let reported_str: String = row.get(3)?;
                    let desired_at_str: Option<String> = row.get(4)?;
                    let reported_at_str: Option<String> = row.get(5)?;
                    let desired_by_str: Option<String> = row.get(6)?;
                    let reconciled: bool = row.get(7)?;

                    Ok(DeviceTwin {
                        device_id,
                        device_type,
                        desired_state: serde_json::from_str(&desired_str).unwrap_or_default(),
                        reported_state: serde_json::from_str(&reported_str).unwrap_or_default(),
                        desired_at: desired_at_str.and_then(|s| {
                            DateTime::parse_from_rfc3339(&s)
                                .ok()
                                .map(|dt| dt.with_timezone(&Utc))
                        }),
                        reported_at: reported_at_str.and_then(|s| {
                            DateTime::parse_from_rfc3339(&s)
                                .ok()
                                .map(|dt| dt.with_timezone(&Utc))
                        }),
                        desired_by: desired_by_str.and_then(|s| serde_json::from_str(&s).ok()),
                        reconciled,
                    })
                })
                .map_err(|e| AgentOSError::HalError(format!("Twin load failed: {e}")))?;

            for twin in rows.flatten() {
                twins.insert(twin.device_id.clone(), twin);
            }
        }

        Ok(Self {
            db: tokio::sync::Mutex::new(conn),
            twins: RwLock::new(twins),
        })
    }

    /// Reconciliation predicate: the twin is reconciled when the reported
    /// state *satisfies* the desired state — every desired key is present
    /// and equal (recursively) in the reported state. Devices routinely
    /// report supersets (extra telemetry like rssi/brightness), so exact
    /// equality would never reconcile a healthy device.
    fn state_satisfies(desired: &Value, reported: &Value) -> bool {
        match (desired, reported) {
            (Value::Object(d), Value::Object(r)) => d
                .iter()
                .all(|(k, v)| r.get(k).is_some_and(|rv| Self::state_satisfies(v, rv))),
            (d, r) => d == r,
        }
    }

    /// Set the desired state for a device (agent-initiated).
    pub async fn set_desired(
        &self,
        device_id: &str,
        device_type: &str,
        state: Value,
        agent_id: &AgentID,
    ) -> Result<(), AgentOSError> {
        let now = Utc::now();
        let state_json = serde_json::to_string(&state)
            .map_err(|e| AgentOSError::HalError(format!("JSON serialize failed: {e}")))?;
        let agent_json = serde_json::to_string(agent_id)
            .map_err(|e| AgentOSError::HalError(format!("Agent ID serialize failed: {e}")))?;

        // One write-lock critical section covering reconciliation, the DB
        // write, and the cache mutation: no concurrent update_reported can
        // slip between the read and write (stale-flag race), and a DB failure
        // leaves the cache untouched (no cache/disk divergence). Lock order
        // is always twins → db, in both update paths.
        let mut twins = self.twins.write().await;
        let reported = twins
            .get(device_id)
            .map(|t| t.reported_state.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let reconciled = Self::state_satisfies(&state, &reported);

        {
            let db = self.db.lock().await;
            db.execute(
                "INSERT INTO device_twins (device_id, device_type, desired_state, desired_at, desired_by, reconciled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(device_id) DO UPDATE SET
                     desired_state = excluded.desired_state,
                     desired_at = excluded.desired_at,
                     desired_by = excluded.desired_by,
                     reconciled = excluded.reconciled",
                params![
                    device_id,
                    device_type,
                    state_json,
                    now.to_rfc3339(),
                    agent_json,
                    reconciled,
                ],
            )
            .map_err(|e| AgentOSError::HalError(format!("Twin desired update failed: {e}")))?;
        }

        let twin = twins.entry(device_id.to_string()).or_insert(DeviceTwin {
            device_id: device_id.to_string(),
            device_type: device_type.to_string(),
            desired_state: Value::Null,
            reported_state: serde_json::json!({}),
            desired_at: None,
            reported_at: None,
            desired_by: None,
            reconciled: true,
        });
        twin.desired_state = state;
        twin.desired_at = Some(now);
        twin.desired_by = Some(*agent_id);
        twin.reconciled = reconciled;

        Ok(())
    }

    /// Update the reported state (from sensor/MQTT callback).
    pub async fn update_reported(
        &self,
        device_id: &str,
        device_type: &str,
        state: Value,
    ) -> Result<(), AgentOSError> {
        let now = Utc::now();
        let state_json = serde_json::to_string(&state)
            .map_err(|e| AgentOSError::HalError(format!("JSON serialize failed: {e}")))?;

        // Same single-critical-section, DB-before-cache discipline as
        // set_desired (lock order: twins → db).
        let mut twins = self.twins.write().await;
        let desired = twins
            .get(device_id)
            .map(|t| t.desired_state.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let reconciled = Self::state_satisfies(&desired, &state);

        {
            let db = self.db.lock().await;
            db.execute(
                "INSERT INTO device_twins (device_id, device_type, reported_state, reported_at, reconciled)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(device_id) DO UPDATE SET
                     reported_state = excluded.reported_state,
                     reported_at = excluded.reported_at,
                     reconciled = excluded.reconciled",
                params![
                    device_id,
                    device_type,
                    state_json,
                    now.to_rfc3339(),
                    reconciled,
                ],
            )
            .map_err(|e| AgentOSError::HalError(format!("Twin reported update failed: {e}")))?;
        }

        let twin = twins.entry(device_id.to_string()).or_insert(DeviceTwin {
            device_id: device_id.to_string(),
            device_type: device_type.to_string(),
            desired_state: serde_json::json!({}),
            reported_state: Value::Null,
            desired_at: None,
            reported_at: None,
            desired_by: None,
            reconciled: true,
        });
        twin.reported_state = state;
        twin.reported_at = Some(now);
        twin.reconciled = reconciled;

        Ok(())
    }

    /// Get the full twin for a device.
    pub async fn get_twin(&self, device_id: &str) -> Option<DeviceTwin> {
        self.twins.read().await.get(device_id).cloned()
    }

    /// List all twins.
    pub async fn list_twins(&self) -> Vec<DeviceTwin> {
        self.twins.read().await.values().cloned().collect()
    }

    /// List unreconciled twins (desired != reported).
    pub async fn list_unreconciled(&self) -> Vec<DeviceTwin> {
        self.twins
            .read()
            .await
            .values()
            .filter(|t| !t.reconciled)
            .cloned()
            .collect()
    }

    /// Get the reported state of a specific device (for safety engine lookups).
    pub async fn get_reported(&self, device_id: &str) -> Option<Value> {
        self.twins
            .read()
            .await
            .get(device_id)
            .map(|t| t.reported_state.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn setup() -> (TwinRegistry, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("twins.db");
        let registry = TwinRegistry::new(&db_path).unwrap();
        (registry, tmp)
    }

    #[tokio::test]
    async fn test_set_and_get_desired() {
        let (reg, _tmp) = setup();
        let agent = AgentID::new();

        reg.set_desired("light.kitchen", "light", json!({"on": true}), &agent)
            .await
            .unwrap();

        let twin = reg.get_twin("light.kitchen").await.unwrap();
        assert_eq!(twin.desired_state, json!({"on": true}));
        assert!(twin.desired_at.is_some());
        assert_eq!(twin.desired_by, Some(agent));
    }

    #[tokio::test]
    async fn test_update_reported() {
        let (reg, _tmp) = setup();

        reg.update_reported("sensor.temp", "sensor", json!({"temperature": 22.5}))
            .await
            .unwrap();

        let twin = reg.get_twin("sensor.temp").await.unwrap();
        assert_eq!(twin.reported_state, json!({"temperature": 22.5}));
        assert!(twin.reported_at.is_some());
    }

    #[tokio::test]
    async fn test_reconciliation_false() {
        let (reg, _tmp) = setup();
        let agent = AgentID::new();

        reg.update_reported("light.kitchen", "light", json!({"on": false}))
            .await
            .unwrap();
        reg.set_desired("light.kitchen", "light", json!({"on": true}), &agent)
            .await
            .unwrap();

        let twin = reg.get_twin("light.kitchen").await.unwrap();
        assert!(!twin.reconciled);
    }

    #[tokio::test]
    async fn test_reconciliation_true() {
        let (reg, _tmp) = setup();
        let agent = AgentID::new();

        reg.update_reported("light.kitchen", "light", json!({"on": true}))
            .await
            .unwrap();
        reg.set_desired("light.kitchen", "light", json!({"on": true}), &agent)
            .await
            .unwrap();

        let twin = reg.get_twin("light.kitchen").await.unwrap();
        assert!(twin.reconciled);
    }

    #[tokio::test]
    async fn test_reconcile_subset() {
        let (reg, _tmp) = setup();
        let agent = AgentID::new();

        reg.set_desired("light.kitchen", "light", json!({"on": true}), &agent)
            .await
            .unwrap();
        // Device reports a superset (extra telemetry) — still reconciled.
        reg.update_reported(
            "light.kitchen",
            "light",
            json!({"on": true, "brightness": 80, "rssi": -40}),
        )
        .await
        .unwrap();

        let twin = reg.get_twin("light.kitchen").await.unwrap();
        assert!(twin.reconciled, "superset reported state must reconcile");
    }

    #[tokio::test]
    async fn test_reconcile_mismatch() {
        let (reg, _tmp) = setup();
        let agent = AgentID::new();

        reg.set_desired("light.kitchen", "light", json!({"on": true}), &agent)
            .await
            .unwrap();
        reg.update_reported("light.kitchen", "light", json!({"on": false, "rssi": -40}))
            .await
            .unwrap();

        let twin = reg.get_twin("light.kitchen").await.unwrap();
        assert!(
            !twin.reconciled,
            "mismatched desired key must not reconcile"
        );
    }

    #[test]
    fn test_state_satisfies_nested() {
        // Nested objects match recursively; scalars compare by equality.
        assert!(TwinRegistry::state_satisfies(
            &json!({"hvac": {"mode": "heat"}}),
            &json!({"hvac": {"mode": "heat", "fan": "auto"}, "extra": 1}),
        ));
        assert!(!TwinRegistry::state_satisfies(
            &json!({"hvac": {"mode": "heat"}}),
            &json!({"hvac": {"mode": "cool"}}),
        ));
        assert!(!TwinRegistry::state_satisfies(
            &json!({"on": true}),
            &json!({})
        ));
        // Empty desired is satisfied by anything.
        assert!(TwinRegistry::state_satisfies(
            &json!({}),
            &json!({"on": false})
        ));
    }

    #[tokio::test]
    async fn test_list_unreconciled() {
        let (reg, _tmp) = setup();
        let agent = AgentID::new();

        reg.update_reported("a", "sensor", json!({"v": 1}))
            .await
            .unwrap();
        reg.set_desired("a", "sensor", json!({"v": 2}), &agent)
            .await
            .unwrap();

        reg.update_reported("b", "sensor", json!({"v": 3}))
            .await
            .unwrap();
        reg.set_desired("b", "sensor", json!({"v": 3}), &agent)
            .await
            .unwrap();

        let unreconciled = reg.list_unreconciled().await;
        assert_eq!(unreconciled.len(), 1);
        assert_eq!(unreconciled[0].device_id, "a");
    }

    #[tokio::test]
    async fn test_persistence() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("twins.db");
        let agent = AgentID::new();

        {
            let reg = TwinRegistry::new(&db_path).unwrap();
            reg.set_desired("light.kitchen", "light", json!({"on": true}), &agent)
                .await
                .unwrap();
        }

        // Re-open and verify data persisted
        let reg = TwinRegistry::new(&db_path).unwrap();
        let twin = reg.get_twin("light.kitchen").await.unwrap();
        assert_eq!(twin.desired_state, json!({"on": true}));
    }
}
