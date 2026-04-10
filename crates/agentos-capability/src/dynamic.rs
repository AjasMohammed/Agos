use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

/// A rule that dynamically grants or revokes permissions based on runtime context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicPermissionRule {
    pub condition: PermissionCondition,
    /// Permissions to add when the condition is met.
    pub grant: Vec<String>,
    /// Permissions to remove when the condition is met.
    pub revoke: Vec<String>,
}

/// Condition that triggers a `DynamicPermissionRule`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionCondition {
    /// Active only within a UTC wall-clock time window.
    TimeWindow { start: NaiveTime, end: NaiveTime },
    /// Active when the agent's budget usage exceeds `max_percent` (0.0–100.0).
    BudgetThreshold { max_percent: f64 },
    /// Active when an escalation approval is pending for this agent.
    EscalationPending,
}

/// Runtime context passed to `DynamicPermissionRule::is_active`.
pub struct DynamicContext {
    pub budget_used_percent: f64,
    pub escalation_pending: bool,
    /// Current UTC wall-clock time used for `TimeWindow` evaluation.
    /// Inject this rather than calling `Utc::now()` inside `is_active` so
    /// that callers can pass a deterministic value in tests.
    pub now: NaiveTime,
}

impl DynamicContext {
    /// Construct a context using the real current UTC time.
    pub fn new(budget_used_percent: f64, escalation_pending: bool) -> Self {
        Self {
            budget_used_percent,
            escalation_pending,
            now: chrono::Utc::now().time(),
        }
    }
}

impl DynamicPermissionRule {
    /// Returns `true` when this rule's condition is satisfied given `ctx`.
    pub fn is_active(&self, ctx: &DynamicContext) -> bool {
        match &self.condition {
            PermissionCondition::TimeWindow { start, end } => {
                let now = ctx.now;
                if start <= end {
                    now >= *start && now <= *end
                } else {
                    // Overnight window: e.g. 22:00 → 06:00
                    now >= *start || now <= *end
                }
            }
            PermissionCondition::BudgetThreshold { max_percent } => {
                ctx.budget_used_percent >= *max_percent
            }
            PermissionCondition::EscalationPending => ctx.escalation_pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_at(budget: f64, escalation: bool, h: u32, m: u32) -> DynamicContext {
        DynamicContext {
            budget_used_percent: budget,
            escalation_pending: escalation,
            now: NaiveTime::from_hms_opt(h, m, 0).unwrap(),
        }
    }

    fn ctx(budget: f64, escalation: bool) -> DynamicContext {
        // Default: midday for deterministic time-unrelated tests
        ctx_at(budget, escalation, 12, 0)
    }

    #[test]
    fn budget_threshold_active_when_exceeded() {
        let rule = DynamicPermissionRule {
            condition: PermissionCondition::BudgetThreshold { max_percent: 80.0 },
            grant: vec!["budget.alert".into()],
            revoke: vec![],
        };
        assert!(rule.is_active(&ctx(90.0, false)));
    }

    #[test]
    fn budget_threshold_inactive_below_limit() {
        let rule = DynamicPermissionRule {
            condition: PermissionCondition::BudgetThreshold { max_percent: 80.0 },
            grant: vec![],
            revoke: vec!["tool.exec".into()],
        };
        assert!(!rule.is_active(&ctx(50.0, false)));
    }

    #[test]
    fn budget_threshold_exact_boundary_is_active() {
        let rule = DynamicPermissionRule {
            condition: PermissionCondition::BudgetThreshold { max_percent: 75.0 },
            grant: vec![],
            revoke: vec![],
        };
        assert!(rule.is_active(&ctx(75.0, false)));
    }

    #[test]
    fn escalation_pending_condition() {
        let rule = DynamicPermissionRule {
            condition: PermissionCondition::EscalationPending,
            grant: vec!["escalation.view".into()],
            revoke: vec!["tool.exec".into()],
        };
        assert!(rule.is_active(&ctx(0.0, true)));
        assert!(!rule.is_active(&ctx(0.0, false)));
    }

    #[test]
    fn time_window_normal_range_active() {
        let rule = DynamicPermissionRule {
            condition: PermissionCondition::TimeWindow {
                start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
            },
            grant: vec!["time.slot".into()],
            revoke: vec![],
        };
        assert!(rule.is_active(&ctx_at(0.0, false, 12, 0))); // midday: active
        assert!(!rule.is_active(&ctx_at(0.0, false, 20, 0))); // evening: inactive
    }

    #[test]
    fn time_window_overnight_range() {
        // Overnight window: 22:00 → 06:00
        let rule = DynamicPermissionRule {
            condition: PermissionCondition::TimeWindow {
                start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
            },
            grant: vec!["night.slot".into()],
            revoke: vec![],
        };
        assert!(rule.is_active(&ctx_at(0.0, false, 23, 30))); // 23:30: active (after start)
        assert!(rule.is_active(&ctx_at(0.0, false, 3, 0))); // 03:00: active (before end)
        assert!(!rule.is_active(&ctx_at(0.0, false, 12, 0))); // midday: inactive
    }

    #[test]
    fn rule_fields_preserved_after_roundtrip() {
        let rule = DynamicPermissionRule {
            condition: PermissionCondition::BudgetThreshold { max_percent: 60.0 },
            grant: vec!["read".into()],
            revoke: vec!["write".into()],
        };
        let json = serde_json::to_string(&rule).unwrap();
        let decoded: DynamicPermissionRule = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.grant, vec!["read"]);
        assert_eq!(decoded.revoke, vec!["write"]);
    }
}
