use super::Hook;
use agentos_types::{HookEvent, HookResult};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Central registry for all lifecycle hooks.
///
/// Hooks are executed in registration order. The first [`HookResult::Abort`]
/// from a Pre-hook short-circuits and skips remaining hooks. For all other
/// event types, every registered hook runs (abort is ignored).
pub struct HookRegistry {
    hooks: RwLock<Vec<Arc<dyn Hook>>>,
}

impl HookRegistry {
    /// Create an empty registry.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            hooks: RwLock::new(Vec::new()),
        })
    }

    /// Register a hook. Later registrations run after earlier ones.
    pub async fn register(&self, hook: Arc<dyn Hook>) {
        self.hooks.write().await.push(hook);
    }

    /// Fire an event through all registered hooks.
    ///
    /// - For **Pre-hooks** (`ToolPre`, `TaskStart`): returns the first
    ///   `Abort` seen and stops processing further hooks.
    /// - For all other events: runs every matching hook and always returns
    ///   `Continue` (abort is meaningless after the fact).
    pub async fn fire(&self, event: &HookEvent) -> HookResult {
        let pre_hook = is_pre_hook(event);
        // Clone Arc pointers under the lock and release immediately.
        // This avoids holding the read lock across async hook executions,
        // preventing both registration stalls and deadlocks if a hook
        // attempts to register another hook inside on_event().
        let hooks: Vec<Arc<dyn Hook>> = self.hooks.read().await.clone();

        for hook in hooks.iter() {
            if !hook.handles(event) {
                continue;
            }
            match hook.on_event(event).await {
                HookResult::Abort(reason) if pre_hook => {
                    tracing::warn!(
                        hook = hook.name(),
                        reason = %reason,
                        "Hook aborted pre-event"
                    );
                    return HookResult::Abort(reason);
                }
                HookResult::Abort(reason) => {
                    // Abort from non-pre hooks is silently treated as Continue.
                    tracing::debug!(
                        hook = hook.name(),
                        reason = %reason,
                        "Hook returned Abort on non-pre event (ignored)"
                    );
                }
                HookResult::Continue => {}
            }
        }
        HookResult::Continue
    }

    /// Return names of all registered hooks (for diagnostics).
    pub async fn list_hook_names(&self) -> Vec<&'static str> {
        self.hooks.read().await.iter().map(|h| h.name()).collect()
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self {
            hooks: RwLock::new(Vec::new()),
        }
    }
}

/// Returns `true` for event variants where `Abort` cancels the operation.
fn is_pre_hook(event: &HookEvent) -> bool {
    matches!(
        event,
        HookEvent::ToolPre { .. } | HookEvent::TaskStart { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, TaskID};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingHook {
        count: Arc<AtomicUsize>,
        result: HookResult,
    }

    impl CountingHook {
        fn new(result: HookResult) -> (Arc<Self>, Arc<AtomicUsize>) {
            let count = Arc::new(AtomicUsize::new(0));
            (
                Arc::new(Self {
                    count: count.clone(),
                    result,
                }),
                count,
            )
        }
    }

    #[async_trait::async_trait]
    impl Hook for CountingHook {
        fn name(&self) -> &'static str {
            "counting-hook"
        }
        fn handles(&self, _: &HookEvent) -> bool {
            true
        }
        async fn on_event(&self, _: &HookEvent) -> HookResult {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    fn tool_pre() -> HookEvent {
        HookEvent::ToolPre {
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            tool_name: "test-tool".to_string(),
            input_json: "{}".to_string(),
        }
    }

    fn task_end() -> HookEvent {
        HookEvent::TaskEnd {
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            success: true,
        }
    }

    #[tokio::test]
    async fn test_all_hooks_fire_on_continue() {
        let registry = HookRegistry::new();
        let (h1, c1) = CountingHook::new(HookResult::Continue);
        let (h2, c2) = CountingHook::new(HookResult::Continue);
        registry.register(h1).await;
        registry.register(h2).await;

        let result = registry.fire(&task_end()).await;
        assert_eq!(result, HookResult::Continue);
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert_eq!(c2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_abort_pre_hook_stops_chain() {
        let registry = HookRegistry::new();
        let (h1, c1) = CountingHook::new(HookResult::Abort("denied".to_string()));
        let (h2, c2) = CountingHook::new(HookResult::Continue);
        registry.register(h1).await;
        registry.register(h2).await;

        let result = registry.fire(&tool_pre()).await;
        assert!(result.is_abort());
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        // Second hook should NOT have fired.
        assert_eq!(c2.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_abort_on_non_pre_hook_is_ignored() {
        let registry = HookRegistry::new();
        let (hook, count) = CountingHook::new(HookResult::Abort("post-abort".to_string()));
        registry.register(hook).await;

        // TaskEnd is not a pre-hook — Abort should be ignored.
        let result = registry.fire(&task_end()).await;
        assert_eq!(result, HookResult::Continue);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_empty_registry_returns_continue() {
        let registry = HookRegistry::new();
        let result = registry.fire(&tool_pre()).await;
        assert_eq!(result, HookResult::Continue);
    }

    #[tokio::test]
    async fn test_list_hook_names() {
        let registry = HookRegistry::new();
        let (h, _) = CountingHook::new(HookResult::Continue);
        registry.register(h).await;
        let names = registry.list_hook_names().await;
        assert_eq!(names, vec!["counting-hook"]);
    }
}
