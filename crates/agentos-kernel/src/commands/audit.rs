use crate::kernel::Kernel;
use agentos_bus::KernelResponse;

impl Kernel {
    /// Export audit chain as JSONL string.
    pub(crate) async fn cmd_export_audit_chain(&self, limit: Option<u32>) -> KernelResponse {
        match self.audit.export_chain_json(limit) {
            Ok(jsonl) => KernelResponse::AuditChainExport(jsonl),
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    /// Resource contention statistics.
    pub(crate) async fn cmd_resource_contention(&self) -> KernelResponse {
        let stats = self.resource_arbiter.contention_stats().await;
        KernelResponse::ResourceContentionStats(stats)
    }
}
