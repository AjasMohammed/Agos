use agentos_types::*;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Per-subscription throttle state tracking.
struct ThrottleState {
    last_delivered: Option<chrono::DateTime<chrono::Utc>>,
    delivery_count_in_window: u32,
    window_start: chrono::DateTime<chrono::Utc>,
}

/// The EventBus is a pure subscription registry and filter evaluator.
/// It does NOT create tasks or call into the kernel — the Kernel orchestrates
/// the full emit → evaluate → create-task flow via `event_dispatch.rs`.
pub struct EventBus {
    subscriptions: RwLock<Vec<EventSubscription>>,
    throttle_state: RwLock<HashMap<SubscriptionID, ThrottleState>>,
    max_chain_depth: u32,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(Vec::new()),
            throttle_state: RwLock::new(HashMap::new()),
            max_chain_depth: 5,
        }
    }

    /// Maximum chain depth before loop detection triggers.
    pub fn max_chain_depth(&self) -> u32 {
        self.max_chain_depth
    }

    // ── Subscription CRUD ─────────────────────────────────────────

    pub async fn subscribe(&self, sub: EventSubscription) -> SubscriptionID {
        let id = sub.id;
        self.subscriptions.write().await.push(sub);
        id
    }

    pub async fn unsubscribe(&self, id: &SubscriptionID) -> bool {
        let mut subs = self.subscriptions.write().await;
        let before = subs.len();
        subs.retain(|s| s.id != *id);
        let removed = subs.len() < before;
        if removed {
            self.throttle_state.write().await.remove(id);
        }
        removed
    }

    pub async fn list_subscriptions(&self) -> Vec<EventSubscription> {
        self.subscriptions.read().await.clone()
    }

    pub async fn list_subscriptions_for_agent(&self, agent_id: &AgentID) -> Vec<EventSubscription> {
        self.subscriptions
            .read()
            .await
            .iter()
            .filter(|s| s.agent_id == *agent_id)
            .cloned()
            .collect()
    }

    pub async fn get_subscription(&self, id: &SubscriptionID) -> Option<EventSubscription> {
        self.subscriptions
            .read()
            .await
            .iter()
            .find(|s| s.id == *id)
            .cloned()
    }

    pub async fn enable_subscription(&self, id: &SubscriptionID) -> bool {
        let mut subs = self.subscriptions.write().await;
        if let Some(sub) = subs.iter_mut().find(|s| s.id == *id) {
            sub.enabled = true;
            true
        } else {
            false
        }
    }

    pub async fn disable_subscription(&self, id: &SubscriptionID) -> bool {
        let mut subs = self.subscriptions.write().await;
        if let Some(sub) = subs.iter_mut().find(|s| s.id == *id) {
            sub.enabled = false;
            true
        } else {
            false
        }
    }

    // ── Subscription evaluation ───────────────────────────────────

    /// Evaluate all subscriptions against an event. Returns matching subscriptions
    /// that pass type filter, enabled check, and throttle policy.
    pub async fn evaluate_subscriptions(&self, event: &EventMessage) -> Vec<EventSubscription> {
        let subs = self.subscriptions.read().await;
        let mut matched = Vec::new();

        for sub in subs.iter() {
            if !sub.enabled {
                continue;
            }
            if !Self::event_matches_type_filter(&event.event_type, &sub.event_type_filter) {
                continue;
            }
            // Phase 1: filter predicates are not evaluated (always pass)
            // Phase 2+ will parse the filter string against the payload
            if self.check_throttle_allowed(&sub.id, &sub.throttle).await {
                matched.push(sub.clone());
            }
        }

        matched
    }

    /// Check if an event type matches a subscription's type filter.
    fn event_matches_type_filter(event_type: &EventType, filter: &EventTypeFilter) -> bool {
        match filter {
            EventTypeFilter::Exact(et) => event_type == et,
            EventTypeFilter::Category(cat) => event_type.category() == *cat,
            EventTypeFilter::All => true,
        }
    }

    /// Check throttle policy. Returns true if delivery is allowed.
    /// Updates throttle state on delivery.
    async fn check_throttle_allowed(
        &self,
        sub_id: &SubscriptionID,
        policy: &ThrottlePolicy,
    ) -> bool {
        match policy {
            ThrottlePolicy::None => {
                self.record_delivery(sub_id).await;
                true
            }
            ThrottlePolicy::MaxOncePerDuration(duration) => {
                let state = self.throttle_state.read().await;
                if let Some(ts) = state.get(sub_id) {
                    if let Some(last) = ts.last_delivered {
                        let elapsed = chrono::Utc::now() - last;
                        let dur_chrono = chrono::Duration::from_std(*duration).unwrap_or_default();
                        if elapsed < dur_chrono {
                            return false;
                        }
                    }
                }
                drop(state);
                self.record_delivery(sub_id).await;
                true
            }
            ThrottlePolicy::MaxCountPerDuration(max_count, duration) => {
                let now = chrono::Utc::now();
                let dur_chrono = chrono::Duration::from_std(*duration).unwrap_or_default();
                let mut state = self.throttle_state.write().await;
                let ts = state.entry(*sub_id).or_insert_with(|| ThrottleState {
                    last_delivered: None,
                    delivery_count_in_window: 0,
                    window_start: now,
                });

                // Reset window if expired
                if now - ts.window_start >= dur_chrono {
                    ts.window_start = now;
                    ts.delivery_count_in_window = 0;
                }

                if ts.delivery_count_in_window >= *max_count {
                    return false;
                }

                ts.delivery_count_in_window += 1;
                ts.last_delivered = Some(now);
                true
            }
        }
    }

    /// Record that a delivery happened for throttle tracking.
    async fn record_delivery(&self, sub_id: &SubscriptionID) {
        let mut state = self.throttle_state.write().await;
        let ts = state.entry(*sub_id).or_insert_with(|| ThrottleState {
            last_delivered: None,
            delivery_count_in_window: 0,
            window_start: chrono::Utc::now(),
        });
        ts.last_delivered = Some(chrono::Utc::now());
        ts.delivery_count_in_window += 1;
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_subscription(
        agent_id: AgentID,
        filter: EventTypeFilter,
        throttle: ThrottlePolicy,
    ) -> EventSubscription {
        EventSubscription {
            id: SubscriptionID::new(),
            agent_id,
            event_type_filter: filter,
            filter: None,
            priority: SubscriptionPriority::Normal,
            throttle,
            enabled: true,
            created_at: chrono::Utc::now(),
        }
    }

    fn make_event(event_type: EventType) -> EventMessage {
        EventMessage {
            id: EventID::new(),
            event_type,
            source: EventSource::AgentLifecycle,
            payload: serde_json::json!({}),
            severity: EventSeverity::Info,
            timestamp: chrono::Utc::now(),
            signature: vec![],
            trace_id: TraceID::new(),
            chain_depth: 0,
        }
    }

    #[tokio::test]
    async fn test_subscribe_and_list() {
        let bus = EventBus::new();
        let agent = AgentID::new();
        let sub = make_subscription(
            agent,
            EventTypeFilter::Exact(EventType::AgentAdded),
            ThrottlePolicy::None,
        );
        let id = bus.subscribe(sub).await;
        let subs = bus.list_subscriptions().await;
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].id, id);
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let bus = EventBus::new();
        let sub = make_subscription(AgentID::new(), EventTypeFilter::All, ThrottlePolicy::None);
        let id = bus.subscribe(sub).await;
        assert!(bus.unsubscribe(&id).await);
        assert_eq!(bus.list_subscriptions().await.len(), 0);
        assert!(!bus.unsubscribe(&id).await); // already removed
    }

    #[tokio::test]
    async fn test_list_for_agent() {
        let bus = EventBus::new();
        let a1 = AgentID::new();
        let a2 = AgentID::new();
        bus.subscribe(make_subscription(
            a1,
            EventTypeFilter::All,
            ThrottlePolicy::None,
        ))
        .await;
        bus.subscribe(make_subscription(
            a2,
            EventTypeFilter::All,
            ThrottlePolicy::None,
        ))
        .await;
        bus.subscribe(make_subscription(
            a1,
            EventTypeFilter::Exact(EventType::AgentRemoved),
            ThrottlePolicy::None,
        ))
        .await;
        assert_eq!(bus.list_subscriptions_for_agent(&a1).await.len(), 2);
        assert_eq!(bus.list_subscriptions_for_agent(&a2).await.len(), 1);
    }

    #[tokio::test]
    async fn test_enable_disable() {
        let bus = EventBus::new();
        let sub = make_subscription(AgentID::new(), EventTypeFilter::All, ThrottlePolicy::None);
        let id = bus.subscribe(sub).await;

        // Disable
        assert!(bus.disable_subscription(&id).await);
        let event = make_event(EventType::AgentAdded);
        let matched = bus.evaluate_subscriptions(&event).await;
        assert_eq!(matched.len(), 0);

        // Enable
        assert!(bus.enable_subscription(&id).await);
        let matched = bus.evaluate_subscriptions(&event).await;
        assert_eq!(matched.len(), 1);
    }

    #[tokio::test]
    async fn test_exact_filter_match() {
        let bus = EventBus::new();
        bus.subscribe(make_subscription(
            AgentID::new(),
            EventTypeFilter::Exact(EventType::AgentAdded),
            ThrottlePolicy::None,
        ))
        .await;

        // Matching event
        let matched = bus
            .evaluate_subscriptions(&make_event(EventType::AgentAdded))
            .await;
        assert_eq!(matched.len(), 1);

        // Non-matching event
        let matched = bus
            .evaluate_subscriptions(&make_event(EventType::AgentRemoved))
            .await;
        assert_eq!(matched.len(), 0);
    }

    #[tokio::test]
    async fn test_category_filter_match() {
        let bus = EventBus::new();
        bus.subscribe(make_subscription(
            AgentID::new(),
            EventTypeFilter::Category(EventCategory::AgentLifecycle),
            ThrottlePolicy::None,
        ))
        .await;

        // Matching: AgentAdded is in AgentLifecycle
        let matched = bus
            .evaluate_subscriptions(&make_event(EventType::AgentAdded))
            .await;
        assert_eq!(matched.len(), 1);

        // Matching: AgentPermissionGranted is also in AgentLifecycle
        let matched = bus
            .evaluate_subscriptions(&make_event(EventType::AgentPermissionGranted))
            .await;
        assert_eq!(matched.len(), 1);

        // Non-matching: TaskFailed is in TaskLifecycle
        let matched = bus
            .evaluate_subscriptions(&make_event(EventType::TaskFailed))
            .await;
        assert_eq!(matched.len(), 0);
    }

    #[tokio::test]
    async fn test_all_filter_matches_everything() {
        let bus = EventBus::new();
        bus.subscribe(make_subscription(
            AgentID::new(),
            EventTypeFilter::All,
            ThrottlePolicy::None,
        ))
        .await;

        let matched = bus
            .evaluate_subscriptions(&make_event(EventType::CPUSpikeDetected))
            .await;
        assert_eq!(matched.len(), 1);
    }

    #[tokio::test]
    async fn test_throttle_max_once_per_duration() {
        let bus = EventBus::new();
        bus.subscribe(make_subscription(
            AgentID::new(),
            EventTypeFilter::All,
            ThrottlePolicy::MaxOncePerDuration(Duration::from_secs(60)),
        ))
        .await;

        let event = make_event(EventType::AgentAdded);

        // First delivery should pass
        let matched = bus.evaluate_subscriptions(&event).await;
        assert_eq!(matched.len(), 1);

        // Second delivery within window should be throttled
        let matched = bus.evaluate_subscriptions(&event).await;
        assert_eq!(matched.len(), 0);
    }

    #[tokio::test]
    async fn test_throttle_max_count_per_duration() {
        let bus = EventBus::new();
        bus.subscribe(make_subscription(
            AgentID::new(),
            EventTypeFilter::All,
            ThrottlePolicy::MaxCountPerDuration(2, Duration::from_secs(60)),
        ))
        .await;

        let event = make_event(EventType::AgentAdded);

        // First two deliveries pass
        assert_eq!(bus.evaluate_subscriptions(&event).await.len(), 1);
        assert_eq!(bus.evaluate_subscriptions(&event).await.len(), 1);

        // Third should be throttled
        assert_eq!(bus.evaluate_subscriptions(&event).await.len(), 0);
    }

    #[tokio::test]
    async fn test_max_chain_depth() {
        let bus = EventBus::new();
        assert_eq!(bus.max_chain_depth(), 5);
    }
}
