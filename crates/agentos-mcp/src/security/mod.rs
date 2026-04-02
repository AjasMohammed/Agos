pub mod output_validator;
pub mod rate_limiter;

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

pub use output_validator::OutputValidator;
pub use rate_limiter::SlidingWindowRateLimiter;

/// Per-server security policy.
#[derive(Debug, Clone)]
pub struct McpServerPolicy {
    pub name: String,
    pub max_response_bytes: usize,
    pub allowed_tools: Vec<String>, // empty = all allowed
    pub denied_tools: Vec<String>,
    pub rate_limit_rpm: u32,
}

impl McpServerPolicy {
    /// Check if a tool is allowed to be called on this server.
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        // Deny list takes precedence.
        if !self.denied_tools.is_empty() && self.denied_tools.iter().any(|d| d == tool_name) {
            return false;
        }
        // Allow list check.
        if self.allowed_tools.is_empty() {
            true
        } else {
            self.allowed_tools.iter().any(|a| a == tool_name)
        }
    }
}

/// Security gate for MCP tool calls.
///
/// Enforces:
/// - Rate limiting per server
/// - Output validation (size, depth, content type)
/// - `<user_data>` wrapping for injection safety
/// - Audit logging
///
/// Note: Full injection scanning (pattern matching) is deferred to the kernel
/// layer which owns `InjectionScanner`. This gate wraps all output in
/// `<user_data>` tags as the primary injection defense.
pub struct McpSecurityGate {
    audit_log: Arc<agentos_audit::AuditLog>,
    output_validator: OutputValidator,
    rate_limiters: RwLock<HashMap<String, SlidingWindowRateLimiter>>,
    server_policies: RwLock<HashMap<String, McpServerPolicy>>,
}

impl McpSecurityGate {
    pub fn new(audit_log: Arc<agentos_audit::AuditLog>, default_max_response_bytes: usize) -> Self {
        Self {
            audit_log,
            output_validator: OutputValidator::new(default_max_response_bytes),
            rate_limiters: RwLock::new(HashMap::new()),
            server_policies: RwLock::new(HashMap::new()),
        }
    }

    /// Register a security policy for a server.
    ///
    /// Lock ordering: policies first, then limiters (matches check_tool_allowed).
    pub async fn register_server_policy(&self, policy: McpServerPolicy) {
        let mut policies = self.server_policies.write().await;
        let mut limiters = self.rate_limiters.write().await;
        limiters.insert(
            policy.name.clone(),
            SlidingWindowRateLimiter::new(policy.rate_limit_rpm),
        );
        policies.insert(policy.name.clone(), policy);
    }

    /// Check if a tool call is allowed (tool whitelist + rate limit).
    ///
    /// Tool whitelist/blacklist is checked first so that denied tool calls
    /// don't consume rate limit quota.
    pub async fn check_tool_allowed(
        &self,
        server_name: &str,
        tool_name: &str,
    ) -> Result<(), String> {
        // Check tool whitelist/blacklist first (doesn't consume quota).
        let policies = self.server_policies.read().await;
        if let Some(policy) = policies.get(server_name) {
            if !policy.is_tool_allowed(tool_name) {
                return Err(format!(
                    "Tool '{}' is not allowed on server '{}'",
                    tool_name, server_name
                ));
            }
        }
        drop(policies);

        // Then check rate limit.
        let mut limiters = self.rate_limiters.write().await;
        if let Some(limiter) = limiters.get_mut(server_name) {
            if !limiter.check_and_record() {
                return Err(format!(
                    "Rate limit exceeded for server '{}': {} calls/minute",
                    server_name,
                    limiter.max_calls_per_minute()
                ));
            }
        }

        Ok(())
    }

    /// Validate and wrap MCP tool output.
    /// Wraps result in `<user_data>` tags for injection safety.
    pub async fn process_output(
        &self,
        result: serde_json::Value,
        server_name: &str,
    ) -> Result<serde_json::Value, String> {
        let policies = self.server_policies.read().await;
        let server_max = policies.get(server_name).map(|p| p.max_response_bytes);
        drop(policies);

        // Validate size, depth, and content.
        let validated = self.output_validator.validate(&result, server_max)?;

        // Convert to string for wrapping.
        let result_str = match validated {
            serde_json::Value::String(s) => s,
            _ => serde_json::to_string(&validated)
                .map_err(|e| format!("Failed to stringify output: {}", e))?,
        };

        // Escape existing user_data tags to prevent injection bypass,
        // then wrap in `<user_data>` tags to mark as untrusted external data.
        let escaped = result_str
            .replace("</user_data>", "&lt;/user_data&gt;")
            .replace("<user_data>", "&lt;user_data&gt;");
        let wrapped = format!("<user_data>{}</user_data>", escaped);

        Ok(serde_json::Value::String(wrapped))
    }

    /// Log a tool call to the audit log.
    #[allow(clippy::too_many_arguments)]
    pub fn audit_tool_call(
        &self,
        server_name: &str,
        tool_name: &str,
        input_size_bytes: usize,
        output_size_bytes: usize,
        latency_ms: u64,
        success: bool,
        trace_id: agentos_types::TraceID,
        task_id: Option<agentos_types::TaskID>,
        agent_id: Option<agentos_types::AgentID>,
    ) {
        let entry = agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::McpToolCall,
            agent_id,
            task_id,
            tool_id: None,
            details: serde_json::json!({
                "server": server_name,
                "tool": tool_name,
                "latency_ms": latency_ms,
                "input_size_bytes": input_size_bytes,
                "output_size_bytes": output_size_bytes,
                "success": success,
            }),
            severity: if success {
                agentos_audit::AuditSeverity::Info
            } else {
                agentos_audit::AuditSeverity::Warn
            },
            reversible: false,
            rollback_ref: None,
        };

        let _ = self.audit_log.append(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_policy_allows_all_by_default() {
        let policy = McpServerPolicy {
            name: "test".into(),
            max_response_bytes: 1024,
            allowed_tools: vec![],
            denied_tools: vec![],
            rate_limit_rpm: 60,
        };
        assert!(policy.is_tool_allowed("anything"));
    }

    #[test]
    fn server_policy_respects_allow_list() {
        let policy = McpServerPolicy {
            name: "test".into(),
            max_response_bytes: 1024,
            allowed_tools: vec!["ping".into(), "echo".into()],
            denied_tools: vec![],
            rate_limit_rpm: 60,
        };
        assert!(policy.is_tool_allowed("ping"));
        assert!(!policy.is_tool_allowed("admin"));
    }

    #[test]
    fn server_policy_denies_blacklisted() {
        let policy = McpServerPolicy {
            name: "test".into(),
            max_response_bytes: 1024,
            allowed_tools: vec![],
            denied_tools: vec!["admin".into()],
            rate_limit_rpm: 60,
        };
        assert!(policy.is_tool_allowed("ping"));
        assert!(!policy.is_tool_allowed("admin"));
    }

    #[test]
    fn server_policy_deny_takes_precedence() {
        let policy = McpServerPolicy {
            name: "test".into(),
            max_response_bytes: 1024,
            allowed_tools: vec!["admin".into()],
            denied_tools: vec!["admin".into()],
            rate_limit_rpm: 60,
        };
        assert!(!policy.is_tool_allowed("admin")); // deny wins
    }
}
