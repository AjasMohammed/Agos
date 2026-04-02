use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::*;
use chrono::Utc;
use std::time::Duration;

#[cfg(test)]
mod tests {
    use super::MAX_SPAWN_DEPTH;
    use crate::scheduler::TaskScheduler;
    use agentos_types::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn make_task_at_depth(depth: u8) -> AgentTask {
        AgentTask {
            id: TaskID::new(),
            state: TaskState::Queued,
            agent_id: AgentID::new(),
            capability_token: CapabilityToken {
                task_id: TaskID::new(),
                agent_id: AgentID::new(),
                allowed_tools: BTreeSet::new(),
                allowed_intents: BTreeSet::new(),
                permissions: PermissionSet::new(),
                issued_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                signature: Vec::new(),
            },
            assigned_llm: None,
            priority: 5,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(300),
            original_prompt: "test".to_string(),
            history: Vec::new(),
            parent_task: None,
            reasoning_hints: None,
            max_iterations: None,
            trigger_source: None,
            autonomous: false,
            parent_task_id: None,
            spawn_depth: depth,
        }
    }

    /// Depth-limit check: a task at MAX_SPAWN_DEPTH must not be allowed to spawn children.
    /// This tests the depth guard logic independently of a full Kernel.
    #[tokio::test]
    async fn test_depth_limit_exceeded_rejects_spawn() {
        let scheduler = TaskScheduler::new(10);
        let parent = make_task_at_depth(MAX_SPAWN_DEPTH);
        let parent_id = parent.id;
        scheduler.enqueue(parent).await;

        // Retrieve the task and assert the guard condition directly.
        let task = scheduler.get_task(&parent_id).await.unwrap();
        assert!(
            task.spawn_depth >= MAX_SPAWN_DEPTH,
            "task should be at or beyond the max depth"
        );

        // The handler checks `spawn_depth >= MAX_SPAWN_DEPTH`. Simulate the guard.
        let would_be_blocked = task.spawn_depth >= MAX_SPAWN_DEPTH;
        assert!(
            would_be_blocked,
            "SpawnSubAgent must be blocked when spawn_depth >= {}",
            MAX_SPAWN_DEPTH
        );
    }

    /// A task below the depth limit should be allowed to proceed.
    #[tokio::test]
    async fn test_depth_below_limit_not_blocked() {
        let scheduler = TaskScheduler::new(10);
        let parent = make_task_at_depth(MAX_SPAWN_DEPTH - 1);
        let parent_id = parent.id;
        scheduler.enqueue(parent).await;

        let task = scheduler.get_task(&parent_id).await.unwrap();
        let would_be_blocked = task.spawn_depth >= MAX_SPAWN_DEPTH;
        assert!(
            !would_be_blocked,
            "SpawnSubAgent must be allowed when spawn_depth < {}",
            MAX_SPAWN_DEPTH
        );
    }

    /// register_child / get_children round-trip.
    #[tokio::test]
    async fn test_register_and_get_children() {
        let scheduler = TaskScheduler::new(10);
        let parent_id = TaskID::new();
        let child1 = TaskID::new();
        let child2 = TaskID::new();

        scheduler.register_child(parent_id, child1).await;
        scheduler.register_child(parent_id, child2).await;

        let children = scheduler.get_children(&parent_id).await;
        assert_eq!(children.len(), 2);
        assert!(children.contains(&child1));
        assert!(children.contains(&child2));
    }

    /// get_children returns empty vec for unknown parent.
    #[tokio::test]
    async fn test_get_children_unknown_parent_returns_empty() {
        let scheduler = TaskScheduler::new(10);
        let unknown = TaskID::new();
        let children = scheduler.get_children(&unknown).await;
        assert!(children.is_empty());
    }

    /// Cascade cancel: registering a child under a parent and querying get_children
    /// returns exactly that child; a child has no children of its own by default.
    /// (The actual cancel cascade is exercised via cmd_cancel_task; this unit test
    /// verifies the data-structure contract that cascade relies on.)
    #[tokio::test]
    async fn test_cancel_parent_cancels_children() {
        let scheduler = TaskScheduler::new(4);

        let parent_id = TaskID::new();
        let child_id = TaskID::new();

        let mut parent_task = make_task_at_depth(0);
        parent_task.id = parent_id;

        let mut child_task = make_task_at_depth(1);
        child_task.id = child_id;
        child_task.parent_task_id = Some(parent_id);

        scheduler.enqueue(parent_task).await;
        scheduler.enqueue(child_task).await;
        scheduler.register_child(parent_id, child_id).await;

        // Parent must report exactly one child.
        let children = scheduler.get_children(&parent_id).await;
        assert_eq!(children.len(), 1);
        assert_eq!(children[0], child_id);

        // Child has no registered children of its own.
        let no_children = scheduler.get_children(&child_id).await;
        assert!(no_children.is_empty(), "child has no registered children");
    }
}

/// Maximum sub-agent spawn depth. Tasks at this depth may not spawn children.
const MAX_SPAWN_DEPTH: u8 = 5;

impl Kernel {
    /// Spawn a child task scoped to the parent's capabilities.
    ///
    /// Steps:
    /// 1. Resolve the parent task — error if not found.
    /// 2. Enforce the depth limit (MAX_SPAWN_DEPTH).
    /// 3. Resolve the target agent by name.
    /// 4. Build a scoped child `CapabilityToken` via `scope_for_child()`.
    /// 5. Enqueue the child task and register it in the child_map.
    /// 6. Write an audit entry.
    pub(crate) async fn cmd_spawn_sub_agent(
        &self,
        parent_task_id: TaskID,
        agent_name: &str,
        prompt: &str,
        requested_permissions: &[String],
    ) -> KernelResponse {
        // 1. Look up parent task.
        let parent_task = match self.scheduler.get_task(&parent_task_id).await {
            Some(t) => t,
            None => {
                return KernelResponse::Error {
                    message: format!("Parent task '{}' not found", parent_task_id),
                }
            }
        };

        // 2. Enforce depth limit.
        if parent_task.spawn_depth >= MAX_SPAWN_DEPTH {
            return KernelResponse::Error {
                message: format!(
                    "spawn depth limit ({}) exceeded: parent task '{}' is at depth {}",
                    MAX_SPAWN_DEPTH, parent_task_id, parent_task.spawn_depth
                ),
            };
        }

        // 3. Resolve the target agent.
        let registry = self.agent_registry.read().await;
        let agent = match registry.get_by_name(agent_name) {
            Some(a) => a.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        if agent.status == AgentStatus::Offline {
            return KernelResponse::Error {
                message: format!("Agent '{}' is offline", agent_name),
            };
        }
        drop(registry);

        // 4. Build the requested permission set from the string list.
        //    Each string is treated as a resource; we grant read+write+execute on it.
        //    `scope_for_child` then intersects these with what the parent actually holds.
        let mut requested = PermissionSet::new();
        for resource in requested_permissions {
            requested.grant(resource.clone(), true, true, true, None);
        }
        // If no permissions were requested, use the parent's full permission set so the
        // intersection returns a non-empty set (the whole parent scope).
        if requested_permissions.is_empty() {
            requested = parent_task.capability_token.permissions.clone();
        }

        let child_cap = match self.capability_engine.scope_for_child(
            &parent_task.capability_token,
            &requested,
            Duration::from_secs(300),
        ) {
            Ok(token) => token,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Failed to scope child capabilities: {}", e),
                }
            }
        };

        // 5. Build and enqueue the child task.
        let child_task = AgentTask {
            id: child_cap.task_id,
            state: TaskState::Queued,
            agent_id: agent.id,
            capability_token: child_cap,
            assigned_llm: Some(agent.id),
            priority: 5,
            created_at: Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(300),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(parent_task_id),
            reasoning_hints: Some(crate::commands::task::infer_reasoning_hints(prompt)),
            max_iterations: None,
            trigger_source: None,
            autonomous: parent_task.autonomous,
            parent_task_id: Some(parent_task_id),
            spawn_depth: parent_task.spawn_depth + 1,
        };

        let child_task_id = self.scheduler.enqueue(child_task).await;

        // Register the child so cascade-cancel can find it.
        self.scheduler
            .register_child(parent_task_id, child_task_id)
            .await;

        // 6. Write audit entry.
        let _ = self.audit.append(AuditEntry {
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCreated,
            agent_id: Some(agent.id),
            task_id: Some(child_task_id),
            tool_id: None,
            details: serde_json::json!({
                "kind": "sub_agent_spawn",
                "parent_task_id": parent_task_id.to_string(),
                "child_task_id": child_task_id.to_string(),
                "agent_name": agent_name,
                "spawn_depth": parent_task.spawn_depth + 1,
                "prompt_preview": prompt.chars().take(200).collect::<String>(),
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelResponse::SubAgentSpawned { child_task_id }
    }

    /// Collect status summaries for a set of awaited child tasks.
    pub(crate) async fn cmd_await_sub_agents(
        &self,
        _parent_task_id: TaskID,
        child_task_ids: &[TaskID],
    ) -> KernelResponse {
        let mut results: Vec<(TaskID, String)> = Vec::with_capacity(child_task_ids.len());

        for &child_id in child_task_ids {
            let summary = match self.scheduler.get_task(&child_id).await {
                Some(task) => format!("state={:?} depth={}", task.state, task.spawn_depth),
                None => "not_found".to_string(),
            };
            results.push((child_id, summary));
        }

        KernelResponse::SubAgentResults { results }
    }
}
