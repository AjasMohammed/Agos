use crate::event_bus::{parse_event_type_filter, parse_subscription_priority};
use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_event_subscribe(
        &self,
        agent_name: String,
        event_filter: String,
        payload_filter: Option<String>,
        throttle: Option<String>,
        priority: Option<String>,
    ) -> KernelResponse {
        // Resolve agent
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(&agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        drop(registry);

        // Parse event type filter
        let event_type_filter = match parse_event_type_filter(&event_filter) {
            Some(f) => f,
            None => {
                return KernelResponse::Error {
                    message: format!(
                        "Invalid event filter '{}'. Use 'all', 'category:<name>', or an exact event type like 'AgentAdded'",
                        event_filter
                    ),
                }
            }
        };

        // Parse throttle policy
        let throttle_policy = match throttle.as_deref() {
            None | Some("none") => ThrottlePolicy::None,
            Some(s) => match parse_throttle(s) {
                Some(p) => p,
                None => {
                    return KernelResponse::Error {
                        message: format!(
                            "Invalid throttle '{}'. Use 'none', 'once_per:<duration>', or 'max:<count>/<duration>'",
                            s
                        ),
                    }
                }
            },
        };

        // Parse priority
        let sub_priority = match parse_subscription_priority(priority.as_deref()) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!(
                        "Invalid priority '{}'. Use 'critical', 'high', 'normal', or 'low'",
                        priority.as_deref().unwrap_or_default()
                    ),
                };
            }
        };

        let payload_filter = payload_filter.and_then(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let sub = EventSubscription {
            id: SubscriptionID::new(),
            agent_id: agent.id,
            event_type_filter,
            filter: payload_filter.clone(),
            priority: sub_priority,
            throttle: throttle_policy,
            enabled: true,
            created_at: chrono::Utc::now(),
        };

        let sub_id = self.event_bus.subscribe(sub).await;

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::EventSubscriptionCreated,
            agent_id: Some(agent.id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "subscription_id": sub_id.to_string(),
                "event_filter": event_filter,
                "payload_filter": payload_filter,
                "agent_name": agent_name,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::EventSubscriptionId(sub_id.to_string())
    }

    pub(crate) async fn cmd_event_unsubscribe(&self, subscription_id: String) -> KernelResponse {
        let id = match subscription_id.parse::<SubscriptionID>() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid subscription ID: {}", subscription_id),
                }
            }
        };

        if self.event_bus.unsubscribe(&id).await {
            self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::EventSubscriptionRemoved,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "subscription_id": subscription_id,
                }),
                severity: agentos_audit::AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            });

            KernelResponse::Success { data: None }
        } else {
            KernelResponse::Error {
                message: format!("Subscription '{}' not found", subscription_id),
            }
        }
    }

    pub(crate) async fn cmd_event_list_subscriptions(
        &self,
        agent_name: Option<String>,
    ) -> KernelResponse {
        let subs = if let Some(name) = agent_name {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(&name) {
                Some(a) => {
                    let agent_id = a.id;
                    drop(registry);
                    self.event_bus.list_subscriptions_for_agent(&agent_id).await
                }
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent '{}' not found", name),
                    }
                }
            }
        } else {
            self.event_bus.list_subscriptions().await
        };

        let values: Vec<serde_json::Value> = subs
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id.to_string(),
                    "agent_id": s.agent_id.to_string(),
                    "event_type_filter": format!("{:?}", s.event_type_filter),
                    "payload_filter": s.filter,
                    "priority": format!("{:?}", s.priority),
                    "throttle": format!("{:?}", s.throttle),
                    "enabled": s.enabled,
                    "created_at": s.created_at.to_rfc3339(),
                })
            })
            .collect();

        KernelResponse::EventSubscriptionList(values)
    }

    pub(crate) async fn cmd_event_get_subscription(
        &self,
        subscription_id: String,
    ) -> KernelResponse {
        let id = match subscription_id.parse::<SubscriptionID>() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid subscription ID: {}", subscription_id),
                }
            }
        };

        match self.event_bus.get_subscription(&id).await {
            Some(sub) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "id": sub.id.to_string(),
                    "agent_id": sub.agent_id.to_string(),
                    "event_type_filter": format!("{:?}", sub.event_type_filter),
                    "payload_filter": sub.filter,
                    "priority": format!("{:?}", sub.priority),
                    "throttle": format!("{:?}", sub.throttle),
                    "enabled": sub.enabled,
                    "created_at": sub.created_at.to_rfc3339(),
                })),
            },
            None => KernelResponse::Error {
                message: format!("Subscription '{}' not found", subscription_id),
            },
        }
    }

    pub(crate) async fn cmd_event_enable_subscription(
        &self,
        subscription_id: String,
    ) -> KernelResponse {
        let id = match subscription_id.parse::<SubscriptionID>() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid subscription ID: {}", subscription_id),
                }
            }
        };

        if self.event_bus.enable_subscription(&id).await {
            KernelResponse::Success { data: None }
        } else {
            KernelResponse::Error {
                message: format!("Subscription '{}' not found", subscription_id),
            }
        }
    }

    pub(crate) async fn cmd_event_disable_subscription(
        &self,
        subscription_id: String,
    ) -> KernelResponse {
        let id = match subscription_id.parse::<SubscriptionID>() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid subscription ID: {}", subscription_id),
                }
            }
        };

        if self.event_bus.disable_subscription(&id).await {
            KernelResponse::Success { data: None }
        } else {
            KernelResponse::Error {
                message: format!("Subscription '{}' not found", subscription_id),
            }
        }
    }

    pub(crate) async fn cmd_event_history(&self, last: u32) -> KernelResponse {
        // Query audit log for recent EventEmitted entries
        match self.audit.query_recent(last) {
            Ok(entries) => {
                let event_entries: Vec<serde_json::Value> = entries
                    .into_iter()
                    .filter(|e| e.event_type == agentos_audit::AuditEventType::EventEmitted)
                    .map(|e| {
                        serde_json::json!({
                            "timestamp": e.timestamp.to_rfc3339(),
                            "event_type": e.details.get("event_type").cloned().unwrap_or_default(),
                            "event_id": e.details.get("event_id").cloned().unwrap_or_default(),
                            "severity": e.details.get("severity").cloned().unwrap_or_default(),
                            "chain_depth": e.details.get("chain_depth").cloned().unwrap_or_default(),
                        })
                    })
                    .collect();
                KernelResponse::EventHistoryList(event_entries)
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to query event history: {}", e),
            },
        }
    }
}

/// Parse a throttle string like "once_per:30s" or "max:5/60s".
fn parse_throttle(s: &str) -> Option<ThrottlePolicy> {
    if let Some(dur_str) = s.strip_prefix("once_per:") {
        let duration = parse_duration(dur_str)?;
        return Some(ThrottlePolicy::MaxOncePerDuration(duration));
    }

    if let Some(rest) = s.strip_prefix("max:") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() != 2 {
            return None;
        }
        let count: u32 = parts[0].parse().ok()?;
        let duration = parse_duration(parts[1])?;
        return Some(ThrottlePolicy::MaxCountPerDuration(count, duration));
    }

    None
}

/// Parse a duration string like "30s", "5m", "1h".
fn parse_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        let n: u64 = secs.parse().ok()?;
        return Some(std::time::Duration::from_secs(n));
    }
    if let Some(mins) = s.strip_suffix('m') {
        let n: u64 = mins.parse().ok()?;
        return Some(std::time::Duration::from_secs(n * 60));
    }
    if let Some(hours) = s.strip_suffix('h') {
        let n: u64 = hours.parse().ok()?;
        return Some(std::time::Duration::from_secs(n * 3600));
    }
    // Default: try as seconds
    let n: u64 = s.parse().ok()?;
    Some(std::time::Duration::from_secs(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_event_filter_all() {
        assert!(matches!(
            parse_event_type_filter("all"),
            Some(EventTypeFilter::All)
        ));
        assert!(matches!(
            parse_event_type_filter("ALL"),
            Some(EventTypeFilter::All)
        ));
    }

    #[test]
    fn test_parse_event_filter_category() {
        match parse_event_type_filter("category:AgentLifecycle") {
            Some(EventTypeFilter::Category(EventCategory::AgentLifecycle)) => {}
            other => panic!("Expected Category(AgentLifecycle), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_event_filter_exact() {
        match parse_event_type_filter("AgentAdded") {
            Some(EventTypeFilter::Exact(EventType::AgentAdded)) => {}
            other => panic!("Expected Exact(AgentAdded), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_event_filter_invalid() {
        assert!(parse_event_type_filter("NotAnEvent").is_none());
        assert!(parse_event_type_filter("category:NotACategory").is_none());
    }

    #[test]
    fn test_parse_throttle_once_per() {
        match parse_throttle("once_per:30s") {
            Some(ThrottlePolicy::MaxOncePerDuration(d)) => {
                assert_eq!(d, std::time::Duration::from_secs(30));
            }
            other => panic!("Expected MaxOncePerDuration(30s), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_throttle_max_count() {
        match parse_throttle("max:5/60s") {
            Some(ThrottlePolicy::MaxCountPerDuration(5, d)) => {
                assert_eq!(d, std::time::Duration::from_secs(60));
            }
            other => panic!("Expected MaxCountPerDuration(5, 60s), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_throttle_invalid() {
        assert!(parse_throttle("invalid").is_none());
        assert!(parse_throttle("max:abc/30s").is_none());
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(
            parse_duration("30s"),
            Some(std::time::Duration::from_secs(30))
        );
        assert_eq!(
            parse_duration("5m"),
            Some(std::time::Duration::from_secs(300))
        );
        assert_eq!(
            parse_duration("1h"),
            Some(std::time::Duration::from_secs(3600))
        );
        assert_eq!(
            parse_duration("10"),
            Some(std::time::Duration::from_secs(10))
        );
        assert!(parse_duration("abc").is_none());
    }
}
