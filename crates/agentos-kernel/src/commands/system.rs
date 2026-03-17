use crate::kernel::Kernel;
use agentos_bus::{KernelResponse, SystemStatus};

impl Kernel {
    pub(crate) async fn cmd_get_status(&self) -> KernelResponse {
        let uptime = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .num_seconds() as u64;
        let connected_agents = self.agent_registry.read().await.list_online().len() as u32;
        let active_tasks = self.scheduler.running_count().await as u32;
        let installed_tools = self.tool_registry.read().await.list_all().len() as u32;

        KernelResponse::Status(SystemStatus {
            uptime_secs: uptime,
            connected_agents,
            active_tasks,
            installed_tools,
            total_audit_entries: 0,
        })
    }

    pub(crate) async fn cmd_get_audit_logs(&self, limit: u32) -> KernelResponse {
        match self.audit.query_recent(limit) {
            Ok(logs) => KernelResponse::AuditLogs(logs),
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }
}
