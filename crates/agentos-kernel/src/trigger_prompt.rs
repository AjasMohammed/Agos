use agentos_types::*;

use crate::kernel::Kernel;

struct AgentPromptInfo {
    name: String,
    model: String,
    role: String,
    permissions: String,
}

impl Kernel {
    /// Build a rich trigger prompt for a given event and subscription.
    pub(crate) async fn build_trigger_prompt(
        &self,
        event: &EventMessage,
        sub: &EventSubscription,
    ) -> String {
        match event.event_type {
            EventType::AgentAdded => self.build_agent_added_prompt(event, sub).await,
            EventType::AgentRemoved => self.build_agent_removed_prompt(event, sub).await,
            EventType::AgentPermissionGranted => {
                self.build_permission_granted_prompt(event, sub).await
            }
            EventType::AgentPermissionRevoked => {
                self.build_permission_revoked_prompt(event, sub).await
            }
            _ => self.build_generic_prompt(event, sub).await,
        }
    }

    // ── AgentAdded ────────────────────────────────────────────────

    async fn build_agent_added_prompt(
        &self,
        event: &EventMessage,
        sub: &EventSubscription,
    ) -> String {
        let agent_name = event
            .payload
            .get("agent_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let agent_model = event
            .payload
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let agent_info = self.get_agent_info_for_prompt(&sub.agent_id).await;
        let os_state = self.build_os_state_snapshot().await;
        let agent_directory = self.build_agent_directory(&sub.agent_id).await;

        format!(
            r#"[SYSTEM CONTEXT]
You are {subscriber_name}, a {subscriber_model} AI agent running inside AgentOS — an agent-native operating system designed for LLMs as primary users.

Your Agent ID: {subscriber_id}
Your Role: {subscriber_role}

Your current permissions:
{subscriber_permissions}

[EVENT NOTIFICATION]
A new agent has been added to this AgentOS instance.

New agent name: {agent_name}
New agent model: {agent_model}
Event time: {timestamp}

[CURRENT OS STATE]
{os_state}

Agents currently active:
{agent_directory}

[AVAILABLE ACTIONS]
You may:
  - Use memory-write to store notes about this new agent
  - Use agent-message to introduce yourself or coordinate with the new agent
  - Emit no intents at all — silence is a valid response

[GUIDANCE]
Consider whether this new agent's capabilities overlap or complement yours.
If you are an orchestrator, consider assigning initial work to this agent.

[RESPONSE EXPECTATION]
This is an informational event. Respond with any coordination actions you want to take, or emit nothing if no action is needed."#,
            subscriber_name = agent_info.name,
            subscriber_model = agent_info.model,
            subscriber_id = sub.agent_id,
            subscriber_role = agent_info.role,
            subscriber_permissions = agent_info.permissions,
            agent_name = agent_name,
            agent_model = agent_model,
            timestamp = event.timestamp.to_rfc3339(),
            os_state = os_state,
            agent_directory = agent_directory,
        )
    }

    // ── AgentRemoved ──────────────────────────────────────────────

    async fn build_agent_removed_prompt(
        &self,
        event: &EventMessage,
        sub: &EventSubscription,
    ) -> String {
        let removed_name = event
            .payload
            .get("agent_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let agent_info = self.get_agent_info_for_prompt(&sub.agent_id).await;
        let os_state = self.build_os_state_snapshot().await;

        format!(
            r#"[SYSTEM CONTEXT]
You are {subscriber_name} operating inside AgentOS.

[EVENT NOTIFICATION]
An agent has been removed from this AgentOS instance.

Removed agent: {removed_name}
Event time: {timestamp}

[CURRENT OS STATE]
{os_state}

[AVAILABLE ACTIONS]
You may:
  - Check if any tasks delegated to the removed agent need reassignment
  - Notify other agents of the change
  - Take no action if unaffected

[GUIDANCE]
Consider whether any of your workflows depended on the removed agent.

[RESPONSE EXPECTATION]
Take any necessary coordination actions, or acknowledge and continue."#,
            subscriber_name = agent_info.name,
            removed_name = removed_name,
            timestamp = event.timestamp.to_rfc3339(),
            os_state = os_state,
        )
    }

    // ── AgentPermissionGranted ────────────────────────────────────

    async fn build_permission_granted_prompt(
        &self,
        event: &EventMessage,
        sub: &EventSubscription,
    ) -> String {
        let permission = event
            .payload
            .get("permission")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let target_agent = event
            .payload
            .get("agent_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let agent_info = self.get_agent_info_for_prompt(&sub.agent_id).await;

        format!(
            r#"[SYSTEM CONTEXT]
You are {subscriber_name} operating inside AgentOS.

[EVENT NOTIFICATION]
A permission has been granted.

Agent: {target_agent}
New permission: {permission}
Granted at: {timestamp}

[CURRENT OS STATE]
Your updated full permission matrix:
{subscriber_permissions}

[AVAILABLE ACTIONS]
You may now use tools that this new permission unlocks.

[GUIDANCE]
Consider whether any of your currently queued or paused tasks could benefit from this new permission.

[RESPONSE EXPECTATION]
Acknowledge the permission change. If you have tasks that can now proceed, continue them."#,
            subscriber_name = agent_info.name,
            target_agent = target_agent,
            permission = permission,
            timestamp = event.timestamp.to_rfc3339(),
            subscriber_permissions = agent_info.permissions,
        )
    }

    // ── AgentPermissionRevoked ────────────────────────────────────

    async fn build_permission_revoked_prompt(
        &self,
        event: &EventMessage,
        sub: &EventSubscription,
    ) -> String {
        let permission = event
            .payload
            .get("permission")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let target_agent = event
            .payload
            .get("agent_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let agent_info = self.get_agent_info_for_prompt(&sub.agent_id).await;

        format!(
            r#"[SYSTEM CONTEXT]
You are {subscriber_name} operating inside AgentOS.

[EVENT NOTIFICATION]
WARNING — A permission has been revoked.

Agent: {target_agent}
Revoked permission: {permission}
Revoked at: {timestamp}

[CURRENT OS STATE]
Your updated full permission matrix:
{subscriber_permissions}

[AVAILABLE ACTIONS]
You may:
  - Adjust your current strategy if the revoked permission affects your tasks
  - Escalate if you believe the revocation was incorrect

[GUIDANCE]
If this is your own permission being revoked, check whether any active tasks depend on it.

[RESPONSE EXPECTATION]
Acknowledge the change. Adjust plans if affected, or continue as normal."#,
            subscriber_name = agent_info.name,
            target_agent = target_agent,
            permission = permission,
            timestamp = event.timestamp.to_rfc3339(),
            subscriber_permissions = agent_info.permissions,
        )
    }

    // ── Generic fallback ──────────────────────────────────────────

    async fn build_generic_prompt(&self, event: &EventMessage, sub: &EventSubscription) -> String {
        let agent_info = self.get_agent_info_for_prompt(&sub.agent_id).await;
        let os_state = self.build_os_state_snapshot().await;

        format!(
            r#"[SYSTEM CONTEXT]
You are {subscriber_name} operating inside AgentOS.

[EVENT NOTIFICATION]
Event: {event_type}
Category: {category}
Severity: {severity:?}
Time: {timestamp}

Payload:
{payload}

[CURRENT OS STATE]
{os_state}

[AVAILABLE ACTIONS]
Respond using your available tools and permissions as appropriate.

[GUIDANCE]
Evaluate the event and decide whether action is needed based on your role.

[RESPONSE EXPECTATION]
Take appropriate action, report findings, escalate if needed, or acknowledge silently."#,
            subscriber_name = agent_info.name,
            event_type = event.event_type,
            category = event.event_type.category(),
            severity = event.severity,
            timestamp = event.timestamp.to_rfc3339(),
            payload = serde_json::to_string_pretty(&event.payload).unwrap_or_default(),
            os_state = os_state,
        )
    }

    // ── Helpers ───────────────────────────────────────────────────

    async fn get_agent_info_for_prompt(&self, agent_id: &AgentID) -> AgentPromptInfo {
        let registry = self.agent_registry.read().await;
        if let Some(profile) = registry.get_by_id(agent_id) {
            let perms = profile
                .permissions
                .entries
                .iter()
                .map(|e| {
                    let ops = format!(
                        "{}{}{}",
                        if e.read { "r" } else { "-" },
                        if e.write { "w" } else { "-" },
                        if e.execute { "x" } else { "-" },
                    );
                    format!("  - {}: {}", e.resource, ops)
                })
                .collect::<Vec<_>>()
                .join("\n");

            AgentPromptInfo {
                name: profile.name.clone(),
                model: format!("{:?}/{}", profile.provider, profile.model),
                role: if profile.description.is_empty() {
                    "general-purpose agent".to_string()
                } else {
                    profile.description.clone()
                },
                permissions: if perms.is_empty() {
                    "  (no permissions granted)".to_string()
                } else {
                    perms
                },
            }
        } else {
            AgentPromptInfo {
                name: "unknown".to_string(),
                model: "unknown".to_string(),
                role: "unknown".to_string(),
                permissions: "  (agent not found)".to_string(),
            }
        }
    }

    async fn build_os_state_snapshot(&self) -> String {
        let agent_count = self.agent_registry.read().await.list_all().len();
        let task_count = self.scheduler.running_count().await;
        let uptime = (chrono::Utc::now() - self.started_at).num_seconds();

        format!(
            "Connected agents: {}\nActive tasks: {}\nUptime: {}s",
            agent_count, task_count, uptime
        )
    }
}
