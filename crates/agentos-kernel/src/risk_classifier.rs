use agentos_types::{ActionRiskLevel, IntentType};
use std::collections::HashMap;

/// Classifies actions into risk levels based on intent type and target resource.
/// Determines whether approval gates should trigger before execution.
pub struct RiskClassifier {
    /// Custom overrides: (intent_type_str, resource_pattern) → risk level
    overrides: HashMap<(String, String), ActionRiskLevel>,
}

impl RiskClassifier {
    pub fn new() -> Self {
        Self {
            overrides: HashMap::new(),
        }
    }

    /// Add a custom risk level override.
    pub fn add_override(
        &mut self,
        intent_type: &str,
        resource_pattern: &str,
        level: ActionRiskLevel,
    ) {
        self.overrides.insert(
            (intent_type.to_string(), resource_pattern.to_string()),
            level,
        );
    }

    /// Classify the risk level of a tool call.
    pub fn classify(
        &self,
        intent_type: IntentType,
        tool_name: &str,
        resource: Option<&str>,
    ) -> ActionRiskLevel {
        let intent_str = format!("{:?}", intent_type).to_lowercase();

        // Check custom overrides first (exact match on intent + tool)
        if let Some(level) = self
            .overrides
            .get(&(intent_str.clone(), tool_name.to_string()))
        {
            return *level;
        }

        // Check resource-based overrides
        if let Some(res) = resource {
            if let Some(level) = self.overrides.get(&(intent_str.clone(), res.to_string())) {
                return *level;
            }
        }

        // Default classification based on intent type and resource patterns
        match intent_type {
            // Level 0: Read-only operations are always autonomous
            IntentType::Read | IntentType::Query | IntentType::Observe => {
                ActionRiskLevel::Autonomous
            }

            // Level 4: Forbidden patterns
            IntentType::Write | IntentType::Execute
                if self.is_forbidden_resource(tool_name, resource) =>
            {
                ActionRiskLevel::Forbidden
            }

            // Level 3: High-risk write targets
            IntentType::Write | IntentType::Execute if self.is_high_risk(tool_name, resource) => {
                ActionRiskLevel::HardApproval
            }

            // Level 2: Moderate-risk writes
            IntentType::Write if self.is_moderate_risk(tool_name, resource) => {
                ActionRiskLevel::SoftApproval
            }

            // Level 1: Low-risk writes (temp dirs, scratchpad)
            IntentType::Write => ActionRiskLevel::Notify,

            // Delegation and agent spawning: Level 3 (hard approval)
            IntentType::Delegate => ActionRiskLevel::HardApproval,

            // Messaging: Level 1 (notify)
            IntentType::Message | IntentType::Broadcast => ActionRiskLevel::Notify,

            // Escalation: Level 0 (agents should always be able to escalate)
            IntentType::Escalate => ActionRiskLevel::Autonomous,

            // Runtime event subscription changes: low operational risk but visible.
            IntentType::Subscribe | IntentType::Unsubscribe => ActionRiskLevel::Notify,

            // Execute: Level 2 by default
            IntentType::Execute => ActionRiskLevel::SoftApproval,
        }
    }

    fn is_forbidden_resource(&self, tool_name: &str, resource: Option<&str>) -> bool {
        let check = |s: &str| {
            s.contains("/etc/")
                || s.contains("/sys/")
                || s.contains("/proc/")
                || s.contains("system-dirs")
                || s.contains("capability.self-escalate")
                || s.contains("secret.read-raw")
        };

        if check(tool_name) {
            return true;
        }
        if let Some(r) = resource {
            if check(r) {
                return true;
            }
        }
        false
    }

    fn is_high_risk(&self, tool_name: &str, resource: Option<&str>) -> bool {
        let high_risk_patterns = [
            "email.send",
            "email-send",
            "social.post",
            "social-post",
            "payment",
            "billing",
            "deploy",
            "publish",
            "delete",
            "rm",
            "remove",
            "agent.spawn",
            "agent-spawn",
        ];

        for pattern in &high_risk_patterns {
            if tool_name.contains(pattern) {
                return true;
            }
            if let Some(r) = resource {
                if r.contains(pattern) {
                    return true;
                }
            }
        }
        false
    }

    fn is_moderate_risk(&self, tool_name: &str, resource: Option<&str>) -> bool {
        let moderate_patterns = [
            "fs.write",
            "file-write",
            "file_write",
            "email.draft",
            "calendar",
            "config",
            "settings",
        ];

        for pattern in &moderate_patterns {
            if tool_name.contains(pattern) {
                return true;
            }
            if let Some(r) = resource {
                if r.contains(pattern) {
                    return true;
                }
            }
        }

        // Any write to user directories
        if let Some(r) = resource {
            if r.contains("/home/") || r.contains("user_data") || r.contains("documents") {
                return true;
            }
        }

        false
    }
}

impl Default for RiskClassifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_is_autonomous() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(IntentType::Read, "file-read", None),
            ActionRiskLevel::Autonomous,
        );
    }

    #[test]
    fn test_query_is_autonomous() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(IntentType::Query, "search", None),
            ActionRiskLevel::Autonomous,
        );
    }

    #[test]
    fn test_email_send_is_hard_approval() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(IntentType::Execute, "email-send", None),
            ActionRiskLevel::HardApproval,
        );
    }

    #[test]
    fn test_system_write_is_forbidden() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(IntentType::Write, "file-write", Some("/etc/passwd")),
            ActionRiskLevel::Forbidden,
        );
    }

    #[test]
    fn test_delegate_is_hard_approval() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(IntentType::Delegate, "task-delegate", None),
            ActionRiskLevel::HardApproval,
        );
    }

    #[test]
    fn test_file_write_user_dir_is_soft_approval() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(
                IntentType::Write,
                "file-write",
                Some("/home/user/report.md")
            ),
            ActionRiskLevel::SoftApproval,
        );
    }

    #[test]
    fn test_escalate_is_autonomous() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(IntentType::Escalate, "escalate", None),
            ActionRiskLevel::Autonomous,
        );
    }

    #[test]
    fn test_custom_override() {
        let mut classifier = RiskClassifier::new();
        classifier.add_override("read", "secret-tool", ActionRiskLevel::HardApproval);
        assert_eq!(
            classifier.classify(IntentType::Read, "secret-tool", None),
            ActionRiskLevel::HardApproval,
        );
    }

    #[test]
    fn test_delete_is_hard_approval() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(IntentType::Execute, "file-delete", None),
            ActionRiskLevel::HardApproval,
        );
    }

    #[test]
    fn test_subscribe_is_notify() {
        let classifier = RiskClassifier::new();
        assert_eq!(
            classifier.classify(IntentType::Subscribe, "event-subscription", None),
            ActionRiskLevel::Notify,
        );
    }
}
