use crate::twin::TwinRegistry;
use agentos_types::AgentOSError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

/// A safety rule violation returned when a desired state change is blocked.
#[derive(Debug, Clone)]
pub struct SafetyViolation {
    pub rule_name: String,
    pub message: String,
    pub requires_escalation: bool,
}

/// A single safety rule defined by the operator in `hardware_limits.toml`.
///
/// Rules use a simple declarative format rather than a full expression language.
/// The safety engine evaluates rules in Rust — the LLM is never in the loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyRule {
    pub name: String,
    pub description: String,
    /// Device type this rule applies to ("*" matches all).
    #[serde(default = "default_wildcard")]
    pub device_type: String,
    /// Action this rule applies to ("set_state", "*").
    #[serde(default = "default_wildcard")]
    pub action: String,
    /// Type of condition to evaluate.
    #[serde(flatten)]
    pub condition: SafetyCondition,
    /// Error message shown to the agent when the rule blocks an action.
    pub error_message: String,
    /// If true, create an escalation for operator approval when blocked.
    #[serde(default)]
    pub escalation: bool,
}

fn default_wildcard() -> String {
    "*".to_string()
}

/// Condition types that can be evaluated by the safety engine.
/// Uses serde's `tag` for TOML-friendly discriminated unions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "condition_type")]
pub enum SafetyCondition {
    /// Block if a reported sensor value crosses a threshold.
    /// Example: block heater if temperature >= 75C.
    #[serde(rename = "reported_threshold")]
    ReportedThreshold {
        /// Device ID to read reported state from.
        sensor_device_id: String,
        /// JSON field name in the reported state.
        field: String,
        /// Comparison operator: "lt", "lte", "gt", "gte", "eq", "neq".
        op: String,
        /// Threshold value.
        threshold: f64,
    },
    /// Block during certain hours (24h format), evaluated in **UTC** —
    /// `allowed_start_hour`/`allowed_end_hour` are compared against
    /// `chrono::Utc::now().hour()`, not the host's local timezone. Convert
    /// local policy hours to UTC when authoring rules.
    /// Example: block door unlock between 23:00 and 06:00 UTC.
    #[serde(rename = "time_window")]
    TimeWindow {
        /// Actions are ALLOWED between start_hour and end_hour (UTC).
        /// Outside this window, the rule blocks.
        allowed_start_hour: u32,
        allowed_end_hour: u32,
    },
}

/// Operator-defined safety rules loaded from TOML config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    #[serde(default)]
    pub rules: Vec<SafetyRule>,
}

/// The safety engine evaluates operator-defined rules against device twin state.
///
/// Written in strictly-typed Rust. The LLM has no influence over rule evaluation.
pub struct SafetyEngine {
    rules: Vec<SafetyRule>,
    twin_registry: Arc<TwinRegistry>,
}

impl SafetyEngine {
    /// Load safety rules from a TOML config file.
    pub fn from_config(
        path: &Path,
        twin_registry: Arc<TwinRegistry>,
    ) -> Result<Self, AgentOSError> {
        let rules = if path.exists() {
            let config_str = std::fs::read_to_string(path).map_err(|e| {
                AgentOSError::HalError(format!("Failed to read safety config: {e}"))
            })?;
            let config: SafetyConfig = toml::from_str(&config_str).map_err(|e| {
                AgentOSError::HalError(format!("Failed to parse safety config: {e}"))
            })?;
            // The only agent-facing actuation path (`hardware-set-desired`)
            // always evaluates with action = "set_state", so a rule keyed on
            // any other action never fires. Surface that loudly.
            for rule in &config.rules {
                if rule.action != "*" && rule.action != "set_state" {
                    tracing::warn!(
                        rule = %rule.name,
                        action = %rule.action,
                        "Safety rule action is neither \"set_state\" nor \"*\" — \
                         it will never match the hardware-set-desired tool and is inert"
                    );
                }
            }
            tracing::info!(count = config.rules.len(), "Loaded safety rules");
            config.rules
        } else {
            tracing::info!("No hardware_limits.toml found — no safety rules active");
            Vec::new()
        };

        Ok(Self {
            rules,
            twin_registry,
        })
    }

    /// Create an engine with explicit rules (for testing).
    pub fn with_rules(rules: Vec<SafetyRule>, twin_registry: Arc<TwinRegistry>) -> Self {
        Self {
            rules,
            twin_registry,
        }
    }

    /// Evaluate all applicable rules for a desired state change.
    ///
    /// Returns `Ok(())` if all rules pass, or `Err(SafetyViolation)` for the
    /// first violated rule.
    pub async fn evaluate(
        &self,
        _device_id: &str,
        device_type: &str,
        action: &str,
        _desired_state: &Value,
    ) -> Result<(), SafetyViolation> {
        for rule in &self.rules {
            // Check if rule applies to this device type
            if rule.device_type != "*" && rule.device_type != device_type {
                continue;
            }
            // Check if rule applies to this action
            if rule.action != "*" && rule.action != action {
                continue;
            }

            if !self.evaluate_condition(&rule.condition).await {
                return Err(SafetyViolation {
                    rule_name: rule.name.clone(),
                    message: rule.error_message.clone(),
                    requires_escalation: rule.escalation,
                });
            }
        }

        Ok(())
    }

    /// Evaluate a single condition. Returns `true` if the condition passes (safe).
    async fn evaluate_condition(&self, condition: &SafetyCondition) -> bool {
        match condition {
            SafetyCondition::ReportedThreshold {
                sensor_device_id,
                field,
                op,
                threshold,
            } => {
                let reported = match self.twin_registry.get_reported(sensor_device_id).await {
                    Some(v) => v,
                    None => {
                        // No data for a guard sensor → we cannot confirm the safe
                        // condition holds, so fail CLOSED (treat as violated) and
                        // block actuation. A safety interlock that can't read its
                        // sensor must never silently permit the action. Matches the
                        // unknown-operator handling below.
                        tracing::warn!(
                            sensor = %sensor_device_id,
                            "Safety guard sensor has no reported state; failing closed (blocking)"
                        );
                        return false;
                    }
                };

                let value = match reported.get(field).and_then(|v| v.as_f64()) {
                    Some(v) => v,
                    None => {
                        tracing::warn!(
                            sensor = %sensor_device_id,
                            rule_field = %field,
                            "Safety guard sensor field missing/non-numeric; failing closed (blocking)"
                        );
                        return false;
                    }
                };

                match op.as_str() {
                    "lt" => value < *threshold,
                    "lte" => value <= *threshold,
                    "gt" => value > *threshold,
                    "gte" => value >= *threshold,
                    "eq" => (value - threshold).abs() < f64::EPSILON,
                    "neq" => (value - threshold).abs() >= f64::EPSILON,
                    _ => {
                        tracing::warn!(
                            op = %op,
                            rule_field = %field,
                            "Unknown comparison operator in safety rule; failing closed (blocking)"
                        );
                        // Unknown op → block. A typo'd operator must never
                        // silently disable an operator-authored safety rule.
                        false
                    }
                }
            }

            SafetyCondition::TimeWindow {
                allowed_start_hour,
                allowed_end_hour,
            } => {
                let hour = chrono::Utc::now().hour();
                if allowed_start_hour <= allowed_end_hour {
                    // Normal window: e.g., 6..23
                    hour >= *allowed_start_hour && hour < *allowed_end_hour
                } else {
                    // Overnight window: e.g., 22..6 (allowed from 22:00 to 05:59)
                    hour >= *allowed_start_hour || hour < *allowed_end_hour
                }
            }
        }
    }

    /// Get a human-readable summary of active rules (for agent context injection).
    pub fn rule_summary(&self) -> Vec<String> {
        self.rules
            .iter()
            .map(|r| format!("{}: {}", r.name, r.description))
            .collect()
    }

    /// Number of loaded rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::twin::TwinRegistry;
    use serde_json::json;
    use tempfile::TempDir;

    fn setup() -> (Arc<TwinRegistry>, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("twins.db");
        let registry = Arc::new(TwinRegistry::new(&db_path).unwrap());
        (registry, tmp)
    }

    #[tokio::test]
    async fn test_threshold_pass() {
        let (twins, _tmp) = setup();
        twins
            .update_reported("sensor_1", "sensor", json!({"temperature": 60.0}))
            .await
            .unwrap();

        let engine = SafetyEngine::with_rules(
            vec![SafetyRule {
                name: "thermal".into(),
                description: "Block heater if temp >= 75".into(),
                device_type: "heater".into(),
                action: "set_state".into(),
                condition: SafetyCondition::ReportedThreshold {
                    sensor_device_id: "sensor_1".into(),
                    field: "temperature".into(),
                    op: "lt".into(),
                    threshold: 75.0,
                },
                error_message: "Too hot".into(),
                escalation: false,
            }],
            twins,
        );

        // 60 < 75 → safe
        assert!(engine
            .evaluate("heater_1", "heater", "set_state", &json!({"on": true}))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_threshold_fail() {
        let (twins, _tmp) = setup();
        twins
            .update_reported("sensor_1", "sensor", json!({"temperature": 80.0}))
            .await
            .unwrap();

        let engine = SafetyEngine::with_rules(
            vec![SafetyRule {
                name: "thermal".into(),
                description: "Block heater if temp >= 75".into(),
                device_type: "heater".into(),
                action: "set_state".into(),
                condition: SafetyCondition::ReportedThreshold {
                    sensor_device_id: "sensor_1".into(),
                    field: "temperature".into(),
                    op: "lt".into(),
                    threshold: 75.0,
                },
                error_message: "Too hot".into(),
                escalation: false,
            }],
            twins,
        );

        // 80 < 75 is false → blocked
        let err = engine
            .evaluate("heater_1", "heater", "set_state", &json!({"on": true}))
            .await
            .unwrap_err();
        assert_eq!(err.rule_name, "thermal");
        assert_eq!(err.message, "Too hot");
    }

    #[tokio::test]
    async fn test_device_type_filter() {
        let (twins, _tmp) = setup();
        twins
            .update_reported("sensor_1", "sensor", json!({"temperature": 80.0}))
            .await
            .unwrap();

        let engine = SafetyEngine::with_rules(
            vec![SafetyRule {
                name: "thermal".into(),
                description: "Only applies to heaters".into(),
                device_type: "heater".into(),
                action: "*".into(),
                condition: SafetyCondition::ReportedThreshold {
                    sensor_device_id: "sensor_1".into(),
                    field: "temperature".into(),
                    op: "lt".into(),
                    threshold: 75.0,
                },
                error_message: "Too hot".into(),
                escalation: false,
            }],
            twins,
        );

        // Rule only applies to "heater", not "light" → passes
        assert!(engine
            .evaluate("light_1", "light", "set_state", &json!({"on": true}))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_escalation_flag() {
        let (twins, _tmp) = setup();
        twins
            .update_reported("sensor_1", "sensor", json!({"temperature": 80.0}))
            .await
            .unwrap();

        let engine = SafetyEngine::with_rules(
            vec![SafetyRule {
                name: "thermal".into(),
                description: "".into(),
                device_type: "*".into(),
                action: "*".into(),
                condition: SafetyCondition::ReportedThreshold {
                    sensor_device_id: "sensor_1".into(),
                    field: "temperature".into(),
                    op: "lt".into(),
                    threshold: 75.0,
                },
                error_message: "Blocked".into(),
                escalation: true,
            }],
            twins,
        );

        let err = engine
            .evaluate("any", "any", "set_state", &json!({}))
            .await
            .unwrap_err();
        assert!(err.requires_escalation);
    }

    #[tokio::test]
    async fn test_unknown_operator_fails_closed() {
        let (twins, _tmp) = setup();
        twins
            .update_reported("sensor_1", "sensor", json!({"temperature": 20.0}))
            .await
            .unwrap();

        let engine = SafetyEngine::with_rules(
            vec![SafetyRule {
                name: "typo".into(),
                description: "operator typo'd the comparison".into(),
                device_type: "*".into(),
                action: "*".into(),
                condition: SafetyCondition::ReportedThreshold {
                    sensor_device_id: "sensor_1".into(),
                    field: "temperature".into(),
                    op: "approx".into(),
                    threshold: 75.0,
                },
                error_message: "Blocked".into(),
                escalation: false,
            }],
            twins,
        );

        // Unknown operator must BLOCK, never silently disable the rule.
        let err = engine
            .evaluate("heater", "heater", "set_state", &json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.rule_name, "typo");
    }

    #[tokio::test]
    async fn test_missing_sensor_data_blocks() {
        let (twins, _tmp) = setup();
        // No sensor data reported

        let engine = SafetyEngine::with_rules(
            vec![SafetyRule {
                name: "thermal".into(),
                description: "".into(),
                device_type: "*".into(),
                action: "*".into(),
                condition: SafetyCondition::ReportedThreshold {
                    sensor_device_id: "nonexistent".into(),
                    field: "temperature".into(),
                    op: "lt".into(),
                    threshold: 75.0,
                },
                error_message: "Blocked".into(),
                escalation: false,
            }],
            twins,
        );

        // No data for a guard sensor → fail CLOSED: a safety interlock that
        // cannot read its sensor must block actuation, not permit it.
        assert!(engine
            .evaluate("heater", "heater", "set_state", &json!({}))
            .await
            .is_err());
    }

    #[test]
    fn test_rule_summary() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("twins.db");
        let twins = Arc::new(TwinRegistry::new(&db_path).unwrap());

        let engine = SafetyEngine::with_rules(
            vec![SafetyRule {
                name: "thermal".into(),
                description: "Block heater when hot".into(),
                device_type: "*".into(),
                action: "*".into(),
                condition: SafetyCondition::TimeWindow {
                    allowed_start_hour: 6,
                    allowed_end_hour: 23,
                },
                error_message: "".into(),
                escalation: false,
            }],
            twins,
        );

        let summary = engine.rule_summary();
        assert_eq!(summary.len(), 1);
        assert!(summary[0].contains("thermal"));
    }

    #[test]
    fn test_toml_deserialization() {
        let toml_str = r#"
[[rules]]
name = "thermal_protection"
description = "Block heater when ambient temp is high"
device_type = "heater"
action = "set_state"
condition_type = "reported_threshold"
sensor_device_id = "thermal_sensor_1"
field = "temperature"
op = "lt"
threshold = 75.0
error_message = "Cannot activate heater: temperature too high"
escalation = false

[[rules]]
name = "night_lock"
description = "Prevent door unlock at night"
device_type = "door_lock"
action = "unlock"
condition_type = "time_window"
allowed_start_hour = 6
allowed_end_hour = 23
error_message = "Door unlock blocked by night lock policy"
escalation = true
"#;

        let config: SafetyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.rules.len(), 2);
        assert_eq!(config.rules[0].name, "thermal_protection");
        assert!(matches!(
            config.rules[0].condition,
            SafetyCondition::ReportedThreshold { .. }
        ));
        assert_eq!(config.rules[1].name, "night_lock");
        assert!(matches!(
            config.rules[1].condition,
            SafetyCondition::TimeWindow { .. }
        ));
        assert!(config.rules[1].escalation);
    }
}
