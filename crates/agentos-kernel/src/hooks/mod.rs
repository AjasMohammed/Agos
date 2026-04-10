pub mod approval_hook;
pub mod audit_hook;
pub mod registry;

pub use approval_hook::{ApprovalHook, AutoApprovePolicy, AutoApproveRule};
pub use audit_hook::AuditHook;
pub use registry::HookRegistry;

use agentos_types::{HookEvent, HookResult};
use async_trait::async_trait;

/// A lifecycle hook that reacts to kernel events.
///
/// Hooks are registered in the [`HookRegistry`] and fired at key lifecycle
/// points (task start/end, tool pre/post, etc.). A hook that returns
/// [`HookResult::Abort`] from a Pre-hook cancels the pending operation.
#[async_trait]
pub trait Hook: Send + Sync {
    /// Human-readable name for this hook (used in logs and diagnostics).
    fn name(&self) -> &'static str;

    /// Return `true` if this hook wants to receive `event`.
    /// Hooks returning `false` are skipped — no async overhead.
    fn handles(&self, event: &HookEvent) -> bool;

    /// Execute the hook logic. Called only when `handles()` returns `true`.
    ///
    /// Returning [`HookResult::Abort`] from a *Pre*-hook (`ToolPre`,
    /// `TaskStart`) causes the kernel to cancel the corresponding operation.
    /// Returning `Abort` from *Post* or informational hooks is treated as
    /// `Continue` — only Pre-hooks can cancel.
    async fn on_event(&self, event: &HookEvent) -> HookResult;
}
