use crate::kernel::Kernel;
use agentos_types::{EventSeverity, EventSource, EventType};

impl Kernel {
    /// List all registered hardware devices.
    pub async fn cmd_hal_list_devices(&self) -> Vec<serde_json::Value> {
        self.hardware_registry
            .list_devices()
            .into_iter()
            .map(|d| {
                serde_json::json!({
                    "id": d.id,
                    "device_type": d.device_type,
                    "status": format!("{:?}", d.status),
                    "granted_to": d.granted_to.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                    "first_seen": d.first_seen,
                    "status_changed_at": d.status_changed_at,
                })
            })
            .collect()
    }

    /// Register a new device in quarantine (idempotent).
    pub async fn cmd_hal_register_device(
        &self,
        device_id: &str,
        device_type: &str,
    ) -> serde_json::Value {
        let is_new = self
            .hardware_registry
            .quarantine_device(device_id, device_type);

        if is_new {
            self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: agentos_types::TraceID::new(),
                event_type: agentos_audit::AuditEventType::HardwareDeviceDetected,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "device_id": device_id,
                    "device_type": device_type,
                    "status": "Quarantined",
                }),
                severity: agentos_audit::AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            });

            self.emit_event(
                EventType::DeviceConnected,
                EventSource::HardwareAbstractionLayer,
                EventSeverity::Info,
                serde_json::json!({
                    "device_id": device_id,
                    "device_type": device_type,
                }),
                0,
            )
            .await;
        }

        serde_json::json!({
            "device_id": device_id,
            "is_new": is_new,
            "status": "Quarantined",
        })
    }

    /// Approve a quarantined device for a specific agent.
    pub async fn cmd_hal_approve_device(
        &self,
        device_id: &str,
        agent_name: &str,
    ) -> serde_json::Value {
        let agent_id = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(agent_name) {
                Some(a) => a.id,
                None => {
                    return serde_json::json!({
                        "error": format!("Agent '{}' not found", agent_name)
                    });
                }
            }
        };

        match self
            .hardware_registry
            .approve_for_agent(device_id, agent_id)
        {
            Ok(()) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::HardwareDeviceApproved,
                    agent_id: Some(agent_id),
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({
                        "device_id": device_id,
                        "agent": agent_name,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: None,
                });
                self.emit_event(
                    EventType::HardwareAccessGranted,
                    EventSource::HardwareAbstractionLayer,
                    EventSeverity::Info,
                    serde_json::json!({
                        "device_id": device_id,
                        "approved_by": "operator",
                        "agent": agent_name,
                    }),
                    0,
                )
                .await;

                serde_json::json!({
                    "status": "approved",
                    "device_id": device_id,
                    "agent": agent_name,
                })
            }
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        }
    }

    /// Deny a device for all agents, clearing any existing grants.
    pub async fn cmd_hal_deny_device(&self, device_id: &str) -> serde_json::Value {
        match self.hardware_registry.deny_device(device_id) {
            Ok(()) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: agentos_types::TraceID::new(),
                    event_type: agentos_audit::AuditEventType::HardwareDeviceDenied,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "device_id": device_id }),
                    severity: agentos_audit::AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
                serde_json::json!({
                    "status": "denied",
                    "device_id": device_id,
                })
            }
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        }
    }

    /// Revoke a specific agent's access to a device.
    pub async fn cmd_hal_revoke_device(
        &self,
        device_id: &str,
        agent_name: &str,
    ) -> serde_json::Value {
        // Verify the device exists before attempting revoke to avoid spurious audit entries.
        let devices = self.hardware_registry.list_devices();
        if !devices.iter().any(|d| d.id == device_id) {
            return serde_json::json!({
                "error": format!("Device '{}' not in registry", device_id)
            });
        }

        let agent_id = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(agent_name) {
                Some(a) => a.id,
                None => {
                    return serde_json::json!({
                        "error": format!("Agent '{}' not found", agent_name)
                    });
                }
            }
        };

        self.hardware_registry
            .revoke_agent_access(device_id, &agent_id);

        self.emit_event(
            EventType::DeviceDisconnected,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Info,
            serde_json::json!({
                "device_id": device_id,
                "reason": "revoked",
                "agent": agent_name,
            }),
            0,
        )
        .await;

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type: agentos_audit::AuditEventType::HardwareDeviceRevoked,
            agent_id: Some(agent_id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "device_id": device_id,
                "agent": agent_name,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });

        serde_json::json!({
            "status": "revoked",
            "device_id": device_id,
            "agent": agent_name,
        })
    }
}
