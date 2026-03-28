use agentos_types::{AgentID, AgentOSError, TaskID};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};

/// Maximum depth of nested RPC calls (A calls B calls C …).
/// Prevents unbounded recursion and circular delegation chains.
const MAX_CALL_DEPTH: u32 = 5;

/// Result returned to the caller when an RPC task completes.
#[derive(Debug, Clone)]
pub struct RpcResult {
    pub output: String,
    pub success: bool,
    pub error: Option<String>,
}

/// A pending RPC call awaiting completion of a child task.
pub struct RpcCall {
    pub caller_task_id: TaskID,
    pub target_agent_id: AgentID,
    pub rpc_task_id: TaskID,
    pub timeout_at: chrono::DateTime<chrono::Utc>,
    /// Oneshot sender to deliver the result back to the blocked caller.
    pub result_tx: oneshot::Sender<RpcResult>,
}

/// Kernel subsystem that tracks pending synchronous RPC calls.
///
/// When agent A uses `agent-call` to invoke agent B, the kernel creates a
/// child task for B and registers the call here. The parent task (A) blocks
/// on a oneshot receiver. When the child completes, `complete_call` sends
/// the result through the oneshot, unblocking the parent.
pub struct RpcManager {
    /// Pending calls keyed by the RPC child task ID.
    pending: Arc<RwLock<HashMap<TaskID, RpcCall>>>,
    /// Call depth per root caller task chain — prevents infinite recursion.
    depths: Arc<RwLock<HashMap<TaskID, u32>>>,
}

impl Default for RpcManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcManager {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            depths: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new RPC call. Returns a oneshot receiver that the caller
    /// blocks on until the child task completes or times out.
    ///
    /// Checks call depth to prevent infinite delegation chains.
    pub async fn register_call(
        &self,
        caller_task_id: TaskID,
        target_agent_id: AgentID,
        rpc_task_id: TaskID,
        timeout_secs: u64,
    ) -> Result<oneshot::Receiver<RpcResult>, AgentOSError> {
        // Check and increment call depth
        let depth = {
            let mut depths = self.depths.write().await;
            let depth = depths.get(&caller_task_id).copied().unwrap_or(0);
            if depth >= MAX_CALL_DEPTH {
                return Err(AgentOSError::RpcDepthExceeded {
                    max: MAX_CALL_DEPTH,
                });
            }
            // The child task inherits the caller's depth + 1
            depths.insert(rpc_task_id, depth + 1);
            depth
        };

        tracing::info!(
            caller_task_id = %caller_task_id,
            target_agent_id = %target_agent_id,
            rpc_task_id = %rpc_task_id,
            depth = depth + 1,
            timeout_secs,
            "Registering RPC call"
        );

        let (tx, rx) = oneshot::channel();
        let timeout_at = chrono::Utc::now() + chrono::Duration::seconds(timeout_secs as i64);

        self.pending.write().await.insert(
            rpc_task_id,
            RpcCall {
                caller_task_id,
                target_agent_id,
                rpc_task_id,
                timeout_at,
                result_tx: tx,
            },
        );

        Ok(rx)
    }

    /// Complete an RPC call by sending the result to the blocked caller.
    /// Returns `true` if a pending call was found and completed.
    pub async fn complete_call(&self, rpc_task_id: &TaskID, result: RpcResult) -> bool {
        if let Some(call) = self.pending.write().await.remove(rpc_task_id) {
            tracing::info!(
                rpc_task_id = %rpc_task_id,
                caller_task_id = %call.caller_task_id,
                success = result.success,
                "Completing RPC call"
            );
            // Clean up depth tracking for the completed child
            self.depths.write().await.remove(rpc_task_id);
            // Send result — if the receiver was dropped (caller timed out), that's fine
            let _ = call.result_tx.send(result);
            true
        } else {
            false
        }
    }

    /// Check if a given task is a pending RPC child task.
    pub async fn is_rpc_task(&self, task_id: &TaskID) -> bool {
        self.pending.read().await.contains_key(task_id)
    }

    /// Sweep expired RPC calls. Returns the list of expired RPC task IDs.
    /// Each expired call receives a timeout error via the oneshot.
    pub async fn sweep_expired(&self) -> Vec<TaskID> {
        let mut pending = self.pending.write().await;
        let now = chrono::Utc::now();

        let expired: Vec<TaskID> = pending
            .iter()
            .filter(|(_, call)| call.timeout_at < now)
            .map(|(id, _)| *id)
            .collect();

        let mut depths = self.depths.write().await;
        for id in &expired {
            if let Some(call) = pending.remove(id) {
                tracing::warn!(
                    rpc_task_id = %id,
                    caller_task_id = %call.caller_task_id,
                    "RPC call timed out"
                );
                let _ = call.result_tx.send(RpcResult {
                    output: String::new(),
                    success: false,
                    error: Some("RPC call timed out".to_string()),
                });
            }
            depths.remove(id);
        }

        expired
    }

    /// Number of currently pending RPC calls.
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_complete_call() {
        let mgr = RpcManager::new();
        let caller = TaskID::new();
        let target = AgentID::new();
        let rpc = TaskID::new();

        let rx = mgr.register_call(caller, target, rpc, 300).await.unwrap();

        assert!(mgr.is_rpc_task(&rpc).await);
        assert_eq!(mgr.pending_count().await, 1);

        mgr.complete_call(
            &rpc,
            RpcResult {
                output: "42".to_string(),
                success: true,
                error: None,
            },
        )
        .await;

        let result = rx.await.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "42");
        assert_eq!(mgr.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_depth_exceeded() {
        let mgr = RpcManager::new();
        let target = AgentID::new();

        // Chain: t0 → t1 → t2 → t3 → t4 → t5 (depth 5 = max, t6 should fail)
        let t0 = TaskID::new();
        let t1 = TaskID::new();
        let t2 = TaskID::new();
        let t3 = TaskID::new();
        let t4 = TaskID::new();
        let t5 = TaskID::new();
        let t6 = TaskID::new();

        let _rx1 = mgr.register_call(t0, target, t1, 300).await.unwrap();
        let _rx2 = mgr.register_call(t1, target, t2, 300).await.unwrap();
        let _rx3 = mgr.register_call(t2, target, t3, 300).await.unwrap();
        let _rx4 = mgr.register_call(t3, target, t4, 300).await.unwrap();
        let _rx5 = mgr.register_call(t4, target, t5, 300).await.unwrap();

        // t5 has depth 5, so calling from t5 should fail
        let result = mgr.register_call(t5, target, t6, 300).await;
        assert!(matches!(
            result,
            Err(AgentOSError::RpcDepthExceeded { max: 5 })
        ));
    }

    #[tokio::test]
    async fn test_sweep_expired() {
        let mgr = RpcManager::new();
        let caller = TaskID::new();
        let target = AgentID::new();
        let rpc = TaskID::new();

        // Register with 0 timeout so it's immediately expired
        let rx = mgr.register_call(caller, target, rpc, 0).await.unwrap();

        // Small sleep to ensure time passes
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let expired = mgr.sweep_expired().await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], rpc);

        let result = rx.await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("timed out"));
    }
}
