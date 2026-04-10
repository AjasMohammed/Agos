use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::hal::HalDriver;

/// HAL driver for Home Assistant REST API integration.
///
/// Allows agents to list entities, query device state, and call services
/// (e.g., turn on lights, set thermostat) through the Home Assistant API.
pub struct HomeAssistantDriver {
    base_url: String,
    /// Access token stored with zeroize-on-drop to prevent memory leakage.
    access_token: ZeroizingToken,
    http_client: reqwest::Client,
}

/// Wrapper that zeroes the token on drop.
struct ZeroizingToken(String);

impl Drop for ZeroizingToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl HomeAssistantDriver {
    /// Create a new Home Assistant driver.
    ///
    /// - `base_url`: Home Assistant instance URL (e.g., "http://homeassistant.local:8123")
    /// - `access_token`: Long-lived access token for authentication
    pub fn new(base_url: &str, access_token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            access_token: ZeroizingToken(access_token.to_string()),
            http_client: reqwest::ClientBuilder::new()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Make an authenticated GET request to the HA API.
    async fn ha_get(&self, path: &str) -> Result<Value, AgentOSError> {
        let url = format!("{}/api{}", self.base_url, path);
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token.0))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| AgentOSError::HalError(format!("Home Assistant GET failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            return Err(AgentOSError::HalError(format!(
                "Home Assistant returned HTTP {status}"
            )));
        }

        resp.json::<Value>()
            .await
            .map_err(|e| AgentOSError::HalError(format!("Invalid JSON from Home Assistant: {e}")))
    }

    /// Make an authenticated POST request to the HA API.
    async fn ha_post(&self, path: &str, body: &Value) -> Result<Value, AgentOSError> {
        let url = format!("{}/api{}", self.base_url, path);
        let resp = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token.0))
            .json(body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| AgentOSError::HalError(format!("Home Assistant POST failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            return Err(AgentOSError::HalError(format!(
                "Home Assistant service call returned HTTP {status}"
            )));
        }

        resp.json::<Value>()
            .await
            .map_err(|e| AgentOSError::HalError(format!("Invalid JSON from Home Assistant: {e}")))
    }
}

/// Validate a Home Assistant entity_id (format: `domain.object_id`).
fn validate_entity_id(entity_id: &str) -> Result<(), AgentOSError> {
    if entity_id.is_empty()
        || !entity_id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return Err(AgentOSError::HalError(
            "Invalid entity_id: must be alphanumeric with dots and underscores (e.g., light.kitchen)"
                .into(),
        ));
    }
    if !entity_id.contains('.') {
        return Err(AgentOSError::HalError(
            "Invalid entity_id: must contain a domain separator (e.g., light.kitchen)".into(),
        ));
    }
    Ok(())
}

/// Validate a HA service domain or service name (alphanumeric + underscore, non-empty).
fn validate_identifier(value: &str, label: &str) -> Result<(), AgentOSError> {
    if value.is_empty() || !value.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err(AgentOSError::HalError(format!(
            "Invalid {label}: must be non-empty alphanumeric with underscores"
        )));
    }
    Ok(())
}

#[async_trait]
impl HalDriver for HomeAssistantDriver {
    fn name(&self) -> &str {
        "homeassistant"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.homeassistant", PermissionOp::Query)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params.get("action").and_then(|v| v.as_str()) {
            Some("call_service") => ("hardware.homeassistant", PermissionOp::Write),
            _ => ("hardware.homeassistant", PermissionOp::Read),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        params
            .get("entity_id")
            .and_then(|v| v.as_str())
            .map(|id| format!("ha:{id}"))
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list_entities");

        match action {
            "list_entities" => {
                let states = self.ha_get("/states").await?;

                let entities: Vec<Value> = states
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|e| {
                                json!({
                                    "entity_id": e.get("entity_id").and_then(|v| v.as_str()).unwrap_or(""),
                                    "state": e.get("state").and_then(|v| v.as_str()).unwrap_or("unknown"),
                                    "friendly_name": e.pointer("/attributes/friendly_name")
                                        .and_then(|v| v.as_str()).unwrap_or(""),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                Ok(json!({ "entities": entities, "count": entities.len() }))
            }

            "get_state" => {
                let entity_id = params
                    .get("entity_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::HalError("Missing 'entity_id' parameter".into())
                    })?;

                validate_entity_id(entity_id)?;

                let state = self.ha_get(&format!("/states/{entity_id}")).await?;
                Ok(state)
            }

            "call_service" => {
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::HalError("Missing 'domain' parameter".into()))?;

                let service = params
                    .get("service")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::HalError("Missing 'service' parameter".into()))?;

                validate_identifier(domain, "domain")?;
                validate_identifier(service, "service")?;

                let data = params.get("data").cloned().unwrap_or(json!({}));

                let result = self
                    .ha_post(&format!("/services/{domain}/{service}"), &data)
                    .await?;
                Ok(result)
            }

            other => Err(AgentOSError::HalError(format!(
                "Unknown Home Assistant action: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_entity_id_ok() {
        assert!(validate_entity_id("light.kitchen").is_ok());
        assert!(validate_entity_id("sensor.temperature_room1").is_ok());
        assert!(validate_entity_id("climate.living_room").is_ok());
    }

    #[test]
    fn test_validate_entity_id_rejects_traversal() {
        assert!(validate_entity_id("../../etc/passwd").is_err());
        assert!(validate_entity_id("light/kitchen").is_err());
    }

    #[test]
    fn test_validate_entity_id_rejects_no_domain() {
        assert!(validate_entity_id("kitchen").is_err());
    }

    #[test]
    fn test_validate_entity_id_rejects_empty() {
        assert!(validate_entity_id("").is_err());
    }

    #[test]
    fn test_validate_entity_id_rejects_control_chars() {
        assert!(validate_entity_id("light.\nkitchen").is_err());
        assert!(validate_entity_id("light.\rkitchen").is_err());
    }

    #[test]
    fn test_validate_identifier_ok() {
        assert!(validate_identifier("light", "domain").is_ok());
        assert!(validate_identifier("turn_on", "service").is_ok());
    }

    #[test]
    fn test_validate_identifier_rejects_empty() {
        assert!(validate_identifier("", "domain").is_err());
    }

    #[test]
    fn test_validate_identifier_rejects_special_chars() {
        assert!(validate_identifier("light/..path", "domain").is_err());
        assert!(validate_identifier("turn on", "service").is_err());
    }

    #[test]
    fn test_device_key() {
        let driver = HomeAssistantDriver::new("http://localhost:8123", "test_token");
        assert_eq!(
            driver.device_key(&json!({ "entity_id": "light.kitchen" })),
            Some("ha:light.kitchen".to_string())
        );
        assert_eq!(driver.device_key(&json!({})), None);
    }

    #[test]
    fn test_permission_read_vs_write() {
        let driver = HomeAssistantDriver::new("http://localhost:8123", "test_token");
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "list_entities" })),
            ("hardware.homeassistant", PermissionOp::Read)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "call_service" })),
            ("hardware.homeassistant", PermissionOp::Write)
        );
    }
}
