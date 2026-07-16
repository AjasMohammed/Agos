//! Policy Engine for Kernel-Mediated Capabilities.
//!
//! Evaluates capability requests against configurable rules to determine
//! whether to Allow, Deny, or Escalate. Rules are prioritized (highest
//! priority checked first) and support domain, action, and resource
//! pattern matching.

use serde::{Deserialize, Serialize};

/// What the policy engine decides for a capability request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    /// Auto-allow without human approval.
    Allow,
    /// Always deny (no escalation, no override).
    Deny,
    /// Require human approval via escalation.
    Escalate,
}

// ---------------------------------------------------------------------------
// Policy rules
// ---------------------------------------------------------------------------

/// A single policy rule that maps capability requests to effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Rule ID for reference in audit logs.
    pub id: String,
    /// Domains this rule applies to (`"*"` = all).
    pub domains: Vec<String>,
    /// Actions this rule applies to (`"*"` = all).
    pub actions: Vec<String>,
    /// Resource pattern (glob-like, `"*"` = all).
    pub resource_pattern: String,
    /// What happens when this rule matches.
    pub effect: PolicyEffect,
    /// Priority (higher number = checked first).
    pub priority: u32,
}

impl PolicyRule {
    /// Check if this rule matches a given request.
    fn matches(&self, domain: &str, action: &str, resource: &str) -> bool {
        let domain_match = self.domains.iter().any(|d| d == "*" || d == domain);
        let action_match = self.actions.iter().any(|a| a == "*" || a == action);
        let resource_match =
            self.resource_pattern == "*" || glob_resource_match(&self.resource_pattern, resource);

        domain_match && action_match && resource_match
    }
}

/// Simple glob matching for resource patterns.
/// Supports `*` as wildcard prefix/suffix matching.
fn glob_resource_match(pattern: &str, resource: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    // Pipe-separated list checked FIRST (higher precedence than wildcards).
    // This ensures "flask|django*" is treated as two alternatives, not a glob.
    if pattern.contains('|') {
        return pattern.split('|').any(|p| p.trim() == resource);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return resource.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return resource.starts_with(prefix);
    }
    pattern == resource
}

// ---------------------------------------------------------------------------
// Policy Engine
// ---------------------------------------------------------------------------

/// The policy engine evaluates capability requests against ordered rules.
///
/// Rules are checked in priority order (highest first). The first matching
/// rule determines the effect. If no rule matches, the default effect applies.
pub struct PolicyEngine {
    rules: Vec<PolicyRule>,
    default_effect: PolicyEffect,
}

impl PolicyEngine {
    /// Create a policy engine with the given rules and default effect.
    pub fn new(mut rules: Vec<PolicyRule>, default_effect: PolicyEffect) -> Self {
        // Sort by priority descending (highest checked first).
        rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
        Self {
            rules,
            default_effect,
        }
    }

    /// Create with default development-friendly rules.
    pub fn development_profile() -> Self {
        let rules = vec![
            // Deny sensitive paths always
            PolicyRule {
                id: "deny-sensitive-paths".into(),
                domains: vec!["storage".into()],
                actions: vec!["*".into()],
                resource_pattern: "/etc/*".into(),
                effect: PolicyEffect::Deny,
                priority: 100,
            },
            PolicyRule {
                id: "deny-ssh-paths".into(),
                domains: vec!["storage".into()],
                actions: vec!["*".into()],
                resource_pattern: "*.ssh/*".into(),
                effect: PolicyEffect::Deny,
                priority: 100,
            },
            // Allow common operations in development
            PolicyRule {
                id: "dev-allow-env".into(),
                domains: vec!["env".into()],
                actions: vec!["*".into()],
                resource_pattern: "*".into(),
                effect: PolicyEffect::Allow,
                priority: 10,
            },
            PolicyRule {
                id: "dev-allow-build".into(),
                domains: vec!["build".into()],
                actions: vec!["*".into()],
                resource_pattern: "*".into(),
                effect: PolicyEffect::Allow,
                priority: 10,
            },
            PolicyRule {
                id: "dev-allow-proc".into(),
                domains: vec!["proc".into()],
                actions: vec!["*".into()],
                resource_pattern: "*".into(),
                effect: PolicyEffect::Allow,
                priority: 10,
            },
        ];
        Self::new(rules, PolicyEffect::Escalate)
    }

    /// Create with production profile — curated allowlists, escalation for unknowns.
    pub fn production_profile() -> Self {
        let rules = vec![
            // Deny sensitive resources
            PolicyRule {
                id: "prod-deny-sensitive".into(),
                domains: vec!["*".into()],
                actions: vec!["*".into()],
                resource_pattern: "/etc/*".into(),
                effect: PolicyEffect::Deny,
                priority: 100,
            },
            // Allow curated Python packages
            PolicyRule {
                id: "prod-allow-python-curated".into(),
                domains: vec!["env".into()],
                actions: vec!["install".into()],
                resource_pattern: "flask|django|fastapi|requests|numpy|pandas|pytest".into(),
                effect: PolicyEffect::Allow,
                priority: 10,
            },
            // Allow build commands
            PolicyRule {
                id: "prod-allow-builds".into(),
                domains: vec!["build".into()],
                actions: vec!["*".into()],
                resource_pattern: "*".into(),
                effect: PolicyEffect::Allow,
                priority: 10,
            },
        ];
        Self::new(rules, PolicyEffect::Escalate)
    }

    /// Create with restricted profile — everything escalated or denied.
    pub fn restricted_profile() -> Self {
        let rules = vec![PolicyRule {
            id: "restricted-deny-all-net".into(),
            domains: vec!["net".into()],
            actions: vec!["*".into()],
            resource_pattern: "*".into(),
            effect: PolicyEffect::Deny,
            priority: 100,
        }];
        Self::new(rules, PolicyEffect::Escalate)
    }

    /// Permissive "off" profile: no rules, default `Allow`. The dynamic policy
    /// layer is wired but inert — it allows everything, exactly matching the
    /// behavior of a kernel that never consulted a policy engine. This is the
    /// safe default so enabling policy enforcement is an explicit operator
    /// opt-in (`[security] policy_profile`), not a silent behavior change.
    pub fn off_profile() -> Self {
        Self::new(vec![], PolicyEffect::Allow)
    }

    /// Build a policy engine from a profile name (`off` | `development` |
    /// `production` | `restricted`). Unknown names fall back to `off` with a
    /// warning so a typo can never accidentally lock down or open up the host.
    pub fn from_profile_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "off" | "" => Self::off_profile(),
            "development" | "dev" => Self::development_profile(),
            "production" | "prod" => Self::production_profile(),
            "restricted" => Self::restricted_profile(),
            other => {
                tracing::warn!(
                    profile = other,
                    "Unknown security.policy_profile; falling back to 'off' (permissive)"
                );
                Self::off_profile()
            }
        }
    }

    /// Evaluate a capability request against all rules.
    ///
    /// Returns the effect of the highest-priority matching rule.
    /// If no rule matches, returns the default effect (typically Escalate).
    pub fn evaluate(&self, domain: &str, action: &str, resource: &str) -> PolicyEffect {
        for rule in &self.rules {
            if rule.matches(domain, action, resource) {
                tracing::debug!(
                    rule_id = %rule.id,
                    domain,
                    action,
                    resource,
                    effect = ?rule.effect,
                    "policy rule matched"
                );
                return rule.effect;
            }
        }
        self.default_effect
    }

    /// Dry-run: evaluate without side effects. Returns the effect and matching rule ID.
    pub fn dry_run(
        &self,
        domain: &str,
        action: &str,
        resource: &str,
    ) -> (PolicyEffect, Option<String>) {
        for rule in &self.rules {
            if rule.matches(domain, action, resource) {
                return (rule.effect, Some(rule.id.clone()));
            }
        }
        (self.default_effect, None)
    }

    /// List all rules.
    pub fn rules(&self) -> &[PolicyRule] {
        &self.rules
    }

    /// Number of rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Replace all rules (e.g., on config reload).
    pub fn replace_rules(&mut self, mut rules: Vec<PolicyRule>) {
        rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
        self.rules = rules;
    }

    /// Default effect when no rule matches.
    pub fn default_effect(&self) -> PolicyEffect {
        self.default_effect
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::development_profile()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_profile_allows_env() {
        let engine = PolicyEngine::development_profile();
        assert_eq!(
            engine.evaluate("env", "install", "flask"),
            PolicyEffect::Allow
        );
    }

    #[test]
    fn development_profile_allows_build() {
        let engine = PolicyEngine::development_profile();
        assert_eq!(
            engine.evaluate("build", "run", "cargo test"),
            PolicyEffect::Allow
        );
    }

    #[test]
    fn development_profile_denies_etc() {
        let engine = PolicyEngine::development_profile();
        assert_eq!(
            engine.evaluate("storage", "zone.create", "/etc/passwd"),
            PolicyEffect::Deny
        );
    }

    #[test]
    fn development_profile_escalates_unknown() {
        let engine = PolicyEngine::development_profile();
        // "net" domain has no explicit rule in dev profile → default escalate
        assert_eq!(
            engine.evaluate("net", "http", "evil.com"),
            PolicyEffect::Escalate
        );
    }

    #[test]
    fn production_profile_allows_curated() {
        let engine = PolicyEngine::production_profile();
        assert_eq!(
            engine.evaluate("env", "install", "flask"),
            PolicyEffect::Allow
        );
        assert_eq!(
            engine.evaluate("env", "install", "numpy"),
            PolicyEffect::Allow
        );
    }

    #[test]
    fn production_profile_escalates_unknown_package() {
        let engine = PolicyEngine::production_profile();
        assert_eq!(
            engine.evaluate("env", "install", "unknown-pkg"),
            PolicyEffect::Escalate
        );
    }

    #[test]
    fn restricted_profile_denies_all_network() {
        let engine = PolicyEngine::restricted_profile();
        assert_eq!(
            engine.evaluate("net", "http", "any-host.com"),
            PolicyEffect::Deny
        );
    }

    #[test]
    fn restricted_profile_escalates_other_domains() {
        let engine = PolicyEngine::restricted_profile();
        assert_eq!(
            engine.evaluate("env", "install", "flask"),
            PolicyEffect::Escalate
        );
    }

    #[test]
    fn priority_ordering() {
        // Higher priority deny should override lower priority allow
        let rules = vec![
            PolicyRule {
                id: "allow-all".into(),
                domains: vec!["*".into()],
                actions: vec!["*".into()],
                resource_pattern: "*".into(),
                effect: PolicyEffect::Allow,
                priority: 1,
            },
            PolicyRule {
                id: "deny-sudo".into(),
                domains: vec!["proc".into()],
                actions: vec!["spawn".into()],
                resource_pattern: "sudo".into(),
                effect: PolicyEffect::Deny,
                priority: 100,
            },
        ];
        let engine = PolicyEngine::new(rules, PolicyEffect::Escalate);

        // sudo should be denied (higher priority)
        assert_eq!(engine.evaluate("proc", "spawn", "sudo"), PolicyEffect::Deny);
        // Other things should be allowed (lower priority catch-all)
        assert_eq!(
            engine.evaluate("proc", "spawn", "python"),
            PolicyEffect::Allow
        );
    }

    #[test]
    fn dry_run_returns_rule_id() {
        let engine = PolicyEngine::development_profile();

        let (effect, rule_id) = engine.dry_run("env", "install", "flask");
        assert_eq!(effect, PolicyEffect::Allow);
        assert_eq!(rule_id.unwrap(), "dev-allow-env");

        let (effect, rule_id) = engine.dry_run("unknown", "unknown", "unknown");
        assert_eq!(effect, PolicyEffect::Escalate);
        assert!(rule_id.is_none());
    }

    #[test]
    fn default_effect_when_no_match() {
        let engine = PolicyEngine::new(vec![], PolicyEffect::Deny);
        assert_eq!(engine.evaluate("any", "thing", "here"), PolicyEffect::Deny);
    }

    #[test]
    fn replace_rules() {
        let mut engine = PolicyEngine::new(vec![], PolicyEffect::Escalate);
        assert_eq!(engine.rule_count(), 0);

        engine.replace_rules(vec![PolicyRule {
            id: "new-rule".into(),
            domains: vec!["*".into()],
            actions: vec!["*".into()],
            resource_pattern: "*".into(),
            effect: PolicyEffect::Allow,
            priority: 1,
        }]);

        assert_eq!(engine.rule_count(), 1);
        assert_eq!(engine.evaluate("any", "thing", "here"), PolicyEffect::Allow);
    }

    #[test]
    fn glob_resource_match_tests() {
        assert!(glob_resource_match("*", "anything"));
        assert!(glob_resource_match("flask", "flask"));
        assert!(!glob_resource_match("flask", "django"));
        assert!(glob_resource_match("*.com", "evil.com"));
        assert!(!glob_resource_match("*.com", "evil.org"));
        assert!(glob_resource_match("/etc/*", "/etc/passwd"));
        assert!(glob_resource_match("flask|django|fastapi", "flask"));
        assert!(glob_resource_match("flask|django|fastapi", "django"));
        assert!(!glob_resource_match("flask|django|fastapi", "numpy"));
    }
}
