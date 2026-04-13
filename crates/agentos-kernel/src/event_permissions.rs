//! Permission gating for agent self-subscription to kernel events.
//!
//! Each [`EventCategory`] maps to a distinct permission resource string.
//! When an agent calls `event-subscribe`, the kernel checks that the agent
//! holds `events.<category>:observe` for the requested filter before
//! creating the subscription. Subscribing to `EventTypeFilter::All` requires
//! observe permission on every category.
//!
//! Kernel-seeded role-based defaults bypass this check (the kernel itself is
//! trusted to seed). Only agent-initiated subscribe calls flow through
//! [`check_subscribe_permission`].

use agentos_types::{
    capability::{PermissionOp, PermissionSet},
    error::AgentOSError,
    event::{EventCategory, EventType, EventTypeFilter},
};

/// Every [`EventCategory`] variant in declaration order. Used by `All` filter
/// checks and by the `event-list-available` tool to enumerate categories.
pub const ALL_EVENT_CATEGORIES: &[EventCategory] = &[
    EventCategory::AgentLifecycle,
    EventCategory::TaskLifecycle,
    EventCategory::SecurityEvents,
    EventCategory::MemoryEvents,
    EventCategory::SystemHealth,
    EventCategory::HardwareEvents,
    EventCategory::ToolEvents,
    EventCategory::AgentCommunication,
    EventCategory::ScheduleEvents,
    EventCategory::ExternalEvents,
];

/// Map an [`EventCategory`] to the permission resource string an agent must
/// hold (with the `Observe` op) in order to subscribe to it.
pub fn permission_for_category(category: EventCategory) -> &'static str {
    match category {
        EventCategory::AgentLifecycle => "events.agent_lifecycle",
        EventCategory::TaskLifecycle => "events.task_lifecycle",
        EventCategory::SecurityEvents => "events.security",
        EventCategory::MemoryEvents => "events.memory",
        EventCategory::SystemHealth => "events.system_health",
        EventCategory::HardwareEvents => "events.hardware",
        EventCategory::ToolEvents => "events.tool",
        EventCategory::AgentCommunication => "events.agent_communication",
        EventCategory::ScheduleEvents => "events.schedule",
        EventCategory::ExternalEvents => "events.external",
    }
}

/// Map an [`EventType`] to its category permission resource.
pub fn permission_for_event(event: &EventType) -> &'static str {
    permission_for_category(event.category())
}

/// Verify that `perms` allows subscribing to events matching `filter`.
///
/// Returns `Err(AgentOSError::PermissionDenied)` for the *first* category
/// that the permission set does not cover, including the resource name and
/// `observe` operation so the agent can interpret the failure.
pub fn check_subscribe_permission(
    perms: &PermissionSet,
    filter: &EventTypeFilter,
) -> Result<(), AgentOSError> {
    match filter {
        EventTypeFilter::Exact(et) => {
            let resource = permission_for_category(et.category());
            if perms.check(resource, PermissionOp::Observe) {
                Ok(())
            } else {
                Err(AgentOSError::PermissionDenied {
                    resource: resource.to_string(),
                    operation: "observe".to_string(),
                })
            }
        }
        EventTypeFilter::Category(cat) => {
            let resource = permission_for_category(*cat);
            if perms.check(resource, PermissionOp::Observe) {
                Ok(())
            } else {
                Err(AgentOSError::PermissionDenied {
                    resource: resource.to_string(),
                    operation: "observe".to_string(),
                })
            }
        }
        EventTypeFilter::All => {
            for cat in ALL_EVENT_CATEGORIES {
                let resource = permission_for_category(*cat);
                if !perms.check(resource, PermissionOp::Observe) {
                    return Err(AgentOSError::PermissionDenied {
                        resource: resource.to_string(),
                        operation: "observe".to_string(),
                    });
                }
            }
            Ok(())
        }
    }
}

/// For each category, return whether `perms` allows subscribing to it.
/// Used by the `event-list-available` tool to surface which categories the
/// caller can act on without having to guess from the permission set.
pub fn subscribable_categories(perms: &PermissionSet) -> Vec<(EventCategory, &'static str, bool)> {
    ALL_EVENT_CATEGORIES
        .iter()
        .map(|cat| {
            let resource = permission_for_category(*cat);
            let allowed = perms.check(resource, PermissionOp::Observe);
            (*cat, resource, allowed)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms_with(resources: &[&str]) -> PermissionSet {
        let mut p = PermissionSet::new();
        for r in resources {
            p.grant_op((*r).to_string(), PermissionOp::Observe, None);
        }
        p
    }

    #[test]
    fn category_permission_strings_are_unique() {
        use std::collections::HashSet;
        let set: HashSet<&str> = ALL_EVENT_CATEGORIES
            .iter()
            .map(|c| permission_for_category(*c))
            .collect();
        assert_eq!(set.len(), ALL_EVENT_CATEGORIES.len());
    }

    #[test]
    fn exact_filter_requires_category_permission() {
        let perms = perms_with(&["events.hardware"]);
        // HardwareEvents → DeviceConnected: allowed
        assert!(check_subscribe_permission(
            &perms,
            &EventTypeFilter::Exact(EventType::DeviceConnected),
        )
        .is_ok());
        // SystemHealth → CPUSpikeDetected: denied
        assert!(matches!(
            check_subscribe_permission(
                &perms,
                &EventTypeFilter::Exact(EventType::CPUSpikeDetected),
            ),
            Err(AgentOSError::PermissionDenied { .. })
        ));
    }

    #[test]
    fn category_filter_requires_matching_permission() {
        let perms = perms_with(&["events.security"]);
        assert!(check_subscribe_permission(
            &perms,
            &EventTypeFilter::Category(EventCategory::SecurityEvents),
        )
        .is_ok());
        assert!(matches!(
            check_subscribe_permission(
                &perms,
                &EventTypeFilter::Category(EventCategory::HardwareEvents),
            ),
            Err(AgentOSError::PermissionDenied { .. })
        ));
    }

    #[test]
    fn all_filter_requires_observe_on_every_category() {
        // Missing one category → fails
        let mut perms = PermissionSet::new();
        for cat in ALL_EVENT_CATEGORIES
            .iter()
            .take(ALL_EVENT_CATEGORIES.len() - 1)
        {
            perms.grant_op(
                permission_for_category(*cat).to_string(),
                PermissionOp::Observe,
                None,
            );
        }
        assert!(matches!(
            check_subscribe_permission(&perms, &EventTypeFilter::All),
            Err(AgentOSError::PermissionDenied { .. })
        ));

        // Grant the last one → succeeds
        perms.grant_op(
            permission_for_category(*ALL_EVENT_CATEGORIES.last().unwrap()).to_string(),
            PermissionOp::Observe,
            None,
        );
        assert!(check_subscribe_permission(&perms, &EventTypeFilter::All).is_ok());
    }

    #[test]
    fn permission_denied_carries_resource_and_observe_op() {
        let perms = PermissionSet::new();
        let err = check_subscribe_permission(
            &perms,
            &EventTypeFilter::Category(EventCategory::HardwareEvents),
        )
        .unwrap_err();
        match err {
            AgentOSError::PermissionDenied {
                resource,
                operation,
            } => {
                assert_eq!(resource, "events.hardware");
                assert_eq!(operation, "observe");
            }
            other => panic!("expected PermissionDenied, got {:?}", other),
        }
    }

    #[test]
    fn subscribable_categories_reports_per_category_state() {
        let perms = perms_with(&["events.memory", "events.tool"]);
        let report = subscribable_categories(&perms);
        assert_eq!(report.len(), ALL_EVENT_CATEGORIES.len());
        let mut allowed: Vec<&'static str> = report
            .into_iter()
            .filter(|(_, _, ok)| *ok)
            .map(|(_, res, _)| res)
            .collect();
        allowed.sort();
        assert_eq!(allowed, vec!["events.memory", "events.tool"]);
    }

    #[test]
    fn observe_op_is_required_not_read() {
        // Granting Read on the resource is not enough — Observe is the gate.
        let mut perms = PermissionSet::new();
        perms.grant("events.hardware".to_string(), true, false, false, None);
        assert!(matches!(
            check_subscribe_permission(
                &perms,
                &EventTypeFilter::Category(EventCategory::HardwareEvents),
            ),
            Err(AgentOSError::PermissionDenied { .. })
        ));
    }
}
