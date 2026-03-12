use crate::kernel::Kernel;

impl Kernel {
    /// List all currently held resource locks.
    pub async fn cmd_resource_list(&self) -> serde_json::Value {
        let locks = self.resource_arbiter.list_locks().await;
        if locks.is_empty() {
            serde_json::json!({ "message": "No resources currently locked.", "locks": [] })
        } else {
            serde_json::json!({ "locks": locks })
        }
    }

    /// Release a specific resource lock held by a given agent.
    pub async fn cmd_resource_release(
        &self,
        resource_id: &str,
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

        self.resource_arbiter.release(resource_id, agent_id).await;

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type: agentos_audit::AuditEventType::PermissionDenied,
            agent_id: Some(agent_id),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "action": "resource_release_forced",
                "resource_id": resource_id,
                "agent": agent_name,
            }),
            severity: agentos_audit::AuditSeverity::Warn,
            reversible: false,
            rollback_ref: None,
        });

        serde_json::json!({
            "status": "released",
            "resource_id": resource_id,
            "agent": agent_name,
        })
    }

    /// Release all locks held by a given agent.
    pub async fn cmd_resource_release_all(&self, agent_name: &str) -> serde_json::Value {
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

        self.resource_arbiter.release_all_for_agent(agent_id).await;

        serde_json::json!({
            "status": "all_released",
            "agent": agent_name,
        })
    }
}
