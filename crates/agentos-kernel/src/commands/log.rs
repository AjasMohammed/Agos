use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::TraceID;

impl Kernel {
    pub(crate) async fn cmd_set_log_level(&self, level: String) -> KernelResponse {
        // Reject pathologically long directives before parsing.
        if level.len() > 512 {
            return KernelResponse::Error {
                message: "Log level directive too long (max 512 chars)".to_string(),
            };
        }

        match crate::logging::apply_log_level(&level) {
            Ok(()) => {
                tracing::info!(level = %level, "Log level updated at runtime");

                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: chrono::Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::KernelConfigChanged,
                    agent_id: None,
                    task_id: None,
                    tool_id: None,
                    details: serde_json::json!({ "setting": "log_level", "value": level }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: None,
                });

                KernelResponse::Success {
                    data: Some(serde_json::json!({ "level": level })),
                }
            }
            Err(e) => {
                tracing::warn!(level = %level, error = %e, "Failed to update log level");
                KernelResponse::Error { message: e }
            }
        }
    }
}
