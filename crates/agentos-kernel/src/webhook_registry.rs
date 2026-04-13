use agentos_types::*;
use chrono::Utc;
use rand::RngCore;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Manages webhook endpoint registrations.
///
/// Each endpoint has a unique ID used in the ingress URL and a secret used for
/// signature verification. Endpoints are persisted to SQLite.
pub struct WebhookRegistry {
    conn: Arc<Mutex<Connection>>,
    /// In-memory cache for fast lookup during ingress (hot path).
    endpoints: RwLock<HashMap<WebhookEndpointID, CachedEndpoint>>,
}

/// In-memory representation with the plaintext secret for signature verification.
#[derive(Clone)]
struct CachedEndpoint {
    pub endpoint: WebhookEndpointMeta,
    pub secret: String,
}

impl WebhookRegistry {
    pub async fn new(db_path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(db_path).map_err(|e| {
            AgentOSError::StorageError(format!("Failed to open webhook registry DB: {e}"))
        })?;

        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;

            CREATE TABLE IF NOT EXISTS webhook_endpoints (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                secret TEXT NOT NULL,
                debounce_seconds INTEGER NOT NULL DEFAULT 0,
                active INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                last_received_at TEXT,
                total_received INTEGER NOT NULL DEFAULT 0
            );
            ",
        )
        .map_err(|e| AgentOSError::StorageError(format!("Failed to create webhook tables: {e}")))?;

        let registry = Self {
            conn: Arc::new(Mutex::new(conn)),
            endpoints: RwLock::new(HashMap::new()),
        };

        // Warm cache from DB
        registry.warm_cache().await?;

        Ok(registry)
    }

    async fn warm_cache(&self) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, provider, secret, debounce_seconds, active,
                        created_at, last_received_at, total_received
                 FROM webhook_endpoints",
            )
            .map_err(|e| {
                AgentOSError::StorageError(format!("Failed to prepare cache warmup: {e}"))
            })?;

        let rows = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                let agent_id_str: String = row.get(1)?;
                let provider_str: String = row.get(2)?;
                let secret: String = row.get(3)?;
                let debounce_seconds: u64 = row.get(4)?;
                let active: bool = row.get(5)?;
                let created_at_str: String = row.get(6)?;
                let last_received_str: Option<String> = row.get(7)?;
                let total_received: u64 = row.get(8)?;

                Ok((
                    id_str,
                    agent_id_str,
                    provider_str,
                    secret,
                    debounce_seconds,
                    active,
                    created_at_str,
                    last_received_str,
                    total_received,
                ))
            })
            .map_err(|e| AgentOSError::StorageError(format!("Failed to query endpoints: {e}")))?;

        let mut cache = self.endpoints.write().await;
        for row in rows {
            let (
                id_str,
                agent_id_str,
                provider_str,
                secret,
                debounce_seconds,
                active,
                created_at_str,
                last_received_str,
                total_received,
            ) = row.map_err(|e| AgentOSError::StorageError(e.to_string()))?;

            let id: WebhookEndpointID = id_str
                .parse()
                .map_err(|_| AgentOSError::StorageError("Invalid endpoint ID".into()))?;
            let agent_id: AgentID = agent_id_str
                .parse()
                .map_err(|_| AgentOSError::StorageError("Invalid agent ID".into()))?;
            let provider: WebhookProvider =
                serde_json::from_str(&provider_str).unwrap_or(WebhookProvider::Generic);
            let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_default();
            let last_received_at = last_received_str.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            });

            cache.insert(
                id,
                CachedEndpoint {
                    endpoint: WebhookEndpointMeta {
                        id,
                        agent_id,
                        provider: provider.to_string(),
                        debounce_seconds,
                        active,
                        created_at,
                        last_received_at,
                        total_received,
                    },
                    secret,
                },
            );
        }

        tracing::info!(count = cache.len(), "Webhook registry cache warmed");
        Ok(())
    }

    /// Create a new webhook endpoint. Returns the endpoint with a generated secret.
    /// Create a new webhook endpoint. Returns the endpoint metadata and the
    /// plaintext signing secret (shown to the user once at creation time).
    pub async fn create_endpoint(
        &self,
        agent_id: AgentID,
        provider: WebhookProvider,
        debounce_seconds: u64,
    ) -> Result<(WebhookEndpointMeta, String), AgentOSError> {
        let id = WebhookEndpointID::new();
        let secret = generate_secret();
        let now = Utc::now();
        let provider_json =
            serde_json::to_string(&provider).unwrap_or_else(|_| "\"generic\"".into());

        {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO webhook_endpoints (id, agent_id, provider, secret, debounce_seconds, active, created_at, total_received)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, 0)",
                params![
                    id.to_string(),
                    agent_id.to_string(),
                    provider_json,
                    secret,
                    debounce_seconds,
                    now.to_rfc3339(),
                ],
            )
            .map_err(|e| AgentOSError::StorageError(format!("Failed to create endpoint: {e}")))?;
        }

        let meta = WebhookEndpointMeta {
            id,
            agent_id,
            provider: provider.to_string(),
            debounce_seconds,
            active: true,
            created_at: now,
            last_received_at: None,
            total_received: 0,
        };

        let secret_copy = secret.clone();
        self.endpoints.write().await.insert(
            id,
            CachedEndpoint {
                endpoint: meta.clone(),
                secret,
            },
        );

        tracing::info!(
            endpoint_id = %id,
            agent_id = %agent_id,
            provider = %meta.provider,
            "Created webhook endpoint"
        );

        Ok((meta, secret_copy))
    }

    /// Look up an endpoint by ID. Returns the metadata and secret for verification.
    pub async fn get_endpoint_with_secret(
        &self,
        id: &WebhookEndpointID,
    ) -> Option<(WebhookEndpointMeta, String)> {
        let cache = self.endpoints.read().await;
        cache
            .get(id)
            .map(|c| (c.endpoint.clone(), c.secret.clone()))
    }

    /// Look up endpoint metadata (no secret).
    pub async fn get_endpoint(&self, id: &WebhookEndpointID) -> Option<WebhookEndpointMeta> {
        let cache = self.endpoints.read().await;
        cache.get(id).map(|c| c.endpoint.clone())
    }

    /// List all endpoints, optionally filtered by agent.
    pub async fn list_endpoints(&self, agent_id: Option<&AgentID>) -> Vec<WebhookEndpointMeta> {
        let cache = self.endpoints.read().await;
        cache
            .values()
            .filter(|c| match agent_id {
                Some(aid) => c.endpoint.agent_id == *aid,
                None => true,
            })
            .map(|c| c.endpoint.clone())
            .collect()
    }

    /// Delete an endpoint.
    pub async fn delete_endpoint(&self, id: &WebhookEndpointID) -> Result<(), AgentOSError> {
        {
            let conn = self.conn.lock().await;
            let deleted = conn
                .execute(
                    "DELETE FROM webhook_endpoints WHERE id = ?1",
                    params![id.to_string()],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to delete endpoint: {e}"))
                })?;

            if deleted == 0 {
                return Err(AgentOSError::StorageError(format!(
                    "Webhook endpoint not found: {id}"
                )));
            }
        }

        self.endpoints.write().await.remove(id);
        tracing::info!(endpoint_id = %id, "Deleted webhook endpoint");
        Ok(())
    }

    /// Rotate the signing secret for an existing endpoint in place.
    /// Returns the new plaintext secret (shown once to the operator).
    pub async fn rotate_secret(&self, id: &WebhookEndpointID) -> Result<String, AgentOSError> {
        let new_secret = generate_secret();
        {
            let conn = self.conn.lock().await;
            let updated = conn
                .execute(
                    "UPDATE webhook_endpoints SET secret = ?1 WHERE id = ?2",
                    params![new_secret, id.to_string()],
                )
                .map_err(|e| {
                    AgentOSError::StorageError(format!("Failed to rotate endpoint secret: {e}"))
                })?;
            if updated == 0 {
                return Err(AgentOSError::StorageError(format!(
                    "Webhook endpoint not found: {id}"
                )));
            }
        }

        let mut cache = self.endpoints.write().await;
        let Some(entry) = cache.get_mut(id) else {
            return Err(AgentOSError::StorageError(format!(
                "Webhook endpoint not found in cache: {id}"
            )));
        };
        entry.secret = new_secret.clone();
        tracing::info!(endpoint_id = %id, "Rotated webhook endpoint secret");
        Ok(new_secret)
    }

    /// Record a webhook receipt — update last_received_at and increment total_received.
    pub async fn record_receipt(&self, id: &WebhookEndpointID) -> Result<(), AgentOSError> {
        let now = Utc::now();

        {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE webhook_endpoints SET last_received_at = ?1, total_received = total_received + 1 WHERE id = ?2",
                params![now.to_rfc3339(), id.to_string()],
            )
            .map_err(|e| AgentOSError::StorageError(format!("Failed to record receipt: {e}")))?;
        }

        // Update cache
        let mut cache = self.endpoints.write().await;
        if let Some(entry) = cache.get_mut(id) {
            entry.endpoint.last_received_at = Some(now);
            entry.endpoint.total_received += 1;
        }

        Ok(())
    }

    /// Get the secret for a webhook endpoint (used during ingress for signature verification).
    pub async fn get_secret(&self, id: &WebhookEndpointID) -> Option<String> {
        let cache = self.endpoints.read().await;
        cache.get(id).map(|c| c.secret.clone())
    }
}

/// Generate a random 32-byte hex secret for webhook signature verification.
fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup() -> (WebhookRegistry, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("webhooks.db");
        let registry = WebhookRegistry::new(&db_path).await.unwrap();
        (registry, tmp)
    }

    #[tokio::test]
    async fn test_create_and_get() {
        let (registry, _tmp) = setup().await;
        let agent_id = AgentID::new();

        let (meta, _secret) = registry
            .create_endpoint(agent_id, WebhookProvider::GitHub, 60)
            .await
            .unwrap();

        assert_eq!(meta.agent_id, agent_id);
        assert_eq!(meta.provider, "github");
        assert_eq!(meta.debounce_seconds, 60);
        assert!(meta.active);
        assert_eq!(meta.total_received, 0);

        let fetched = registry.get_endpoint(&meta.id).await.unwrap();
        assert_eq!(fetched.id, meta.id);
    }

    #[tokio::test]
    async fn test_get_with_secret() {
        let (registry, _tmp) = setup().await;
        let agent_id = AgentID::new();

        let (meta, _secret) = registry
            .create_endpoint(agent_id, WebhookProvider::Generic, 0)
            .await
            .unwrap();

        let (fetched, secret) = registry.get_endpoint_with_secret(&meta.id).await.unwrap();
        assert_eq!(fetched.id, meta.id);
        assert_eq!(secret.len(), 64); // 32 bytes hex-encoded
    }

    #[tokio::test]
    async fn test_list_all() {
        let (registry, _tmp) = setup().await;
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();

        registry
            .create_endpoint(agent_a, WebhookProvider::GitHub, 0)
            .await
            .unwrap();
        registry
            .create_endpoint(agent_b, WebhookProvider::Stripe, 30)
            .await
            .unwrap();

        let all = registry.list_endpoints(None).await;
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_list_by_agent() {
        let (registry, _tmp) = setup().await;
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();

        registry
            .create_endpoint(agent_a, WebhookProvider::GitHub, 0)
            .await
            .unwrap();
        registry
            .create_endpoint(agent_b, WebhookProvider::Stripe, 0)
            .await
            .unwrap();

        let a_endpoints = registry.list_endpoints(Some(&agent_a)).await;
        assert_eq!(a_endpoints.len(), 1);
        assert_eq!(a_endpoints[0].agent_id, agent_a);
    }

    #[tokio::test]
    async fn test_delete() {
        let (registry, _tmp) = setup().await;
        let agent_id = AgentID::new();

        let (meta, _secret) = registry
            .create_endpoint(agent_id, WebhookProvider::GitHub, 0)
            .await
            .unwrap();

        registry.delete_endpoint(&meta.id).await.unwrap();
        assert!(registry.get_endpoint(&meta.id).await.is_none());
    }

    #[tokio::test]
    async fn test_delete_not_found() {
        let (registry, _tmp) = setup().await;
        let fake_id = WebhookEndpointID::new();
        assert!(registry.delete_endpoint(&fake_id).await.is_err());
    }

    #[tokio::test]
    async fn test_record_receipt() {
        let (registry, _tmp) = setup().await;
        let agent_id = AgentID::new();

        let (meta, _secret) = registry
            .create_endpoint(agent_id, WebhookProvider::GitHub, 0)
            .await
            .unwrap();

        registry.record_receipt(&meta.id).await.unwrap();
        registry.record_receipt(&meta.id).await.unwrap();

        let updated = registry.get_endpoint(&meta.id).await.unwrap();
        assert_eq!(updated.total_received, 2);
        assert!(updated.last_received_at.is_some());
    }

    #[tokio::test]
    async fn test_rotate_secret_in_place() {
        let (registry, _tmp) = setup().await;
        let agent_id = AgentID::new();

        let (meta, old_secret) = registry
            .create_endpoint(agent_id, WebhookProvider::Generic, 0)
            .await
            .unwrap();

        let new_secret = registry.rotate_secret(&meta.id).await.unwrap();
        assert_ne!(new_secret, old_secret);

        let fetched = registry.get_endpoint(&meta.id).await.unwrap();
        assert_eq!(fetched.id, meta.id);

        let cached = registry.get_secret(&meta.id).await.unwrap();
        assert_eq!(cached, new_secret);
    }
}
