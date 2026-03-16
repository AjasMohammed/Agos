use agentos_types::*;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::RwLock;

/// Per-subscription throttle state tracking.
struct ThrottleState {
    last_delivered: Option<chrono::DateTime<chrono::Utc>>,
    delivery_count_in_window: u32,
    window_start: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
enum CompiledFilter {
    Parsed(EventFilterExpr),
    Invalid,
}

#[derive(Debug, Clone)]
struct CompiledSubscription {
    subscription: EventSubscription,
    compiled_filter: Option<CompiledFilter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    Contains,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterValue {
    String(String),
    Number(f64),
    Bool(bool),
    List(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterPredicate {
    pub field: String,
    pub op: FilterOp,
    pub value: FilterValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventFilterExpr {
    pub predicates: Vec<FilterPredicate>,
}

/// The EventBus is a pure subscription registry and filter evaluator.
/// It does NOT create tasks or call into the kernel — the Kernel orchestrates
/// the full emit → evaluate → create-task flow via `event_dispatch.rs`.
pub struct EventBus {
    subscriptions: RwLock<Vec<CompiledSubscription>>,
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
        let compiled_filter = Self::compile_filter(&sub);
        self.subscriptions.write().await.push(CompiledSubscription {
            subscription: sub,
            compiled_filter,
        });
        id
    }

    pub async fn unsubscribe(&self, id: &SubscriptionID) -> bool {
        let mut subs = self.subscriptions.write().await;
        let before = subs.len();
        subs.retain(|s| s.subscription.id != *id);
        let removed = subs.len() < before;
        if removed {
            self.throttle_state.write().await.remove(id);
        }
        removed
    }

    pub async fn list_subscriptions(&self) -> Vec<EventSubscription> {
        self.subscriptions
            .read()
            .await
            .iter()
            .map(|s| s.subscription.clone())
            .collect()
    }

    pub async fn list_subscriptions_for_agent(&self, agent_id: &AgentID) -> Vec<EventSubscription> {
        self.subscriptions
            .read()
            .await
            .iter()
            .filter(|s| s.subscription.agent_id == *agent_id)
            .map(|s| s.subscription.clone())
            .collect()
    }

    pub async fn get_subscription(&self, id: &SubscriptionID) -> Option<EventSubscription> {
        self.subscriptions
            .read()
            .await
            .iter()
            .find(|s| s.subscription.id == *id)
            .map(|s| s.subscription.clone())
    }

    pub async fn enable_subscription(&self, id: &SubscriptionID) -> bool {
        let mut subs = self.subscriptions.write().await;
        if let Some(sub) = subs.iter_mut().find(|s| s.subscription.id == *id) {
            sub.subscription.enabled = true;
            true
        } else {
            false
        }
    }

    pub async fn disable_subscription(&self, id: &SubscriptionID) -> bool {
        let mut subs = self.subscriptions.write().await;
        if let Some(sub) = subs.iter_mut().find(|s| s.subscription.id == *id) {
            sub.subscription.enabled = false;
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
            if !sub.subscription.enabled {
                continue;
            }
            if !Self::event_matches_type_filter(
                &event.event_type,
                &sub.subscription.event_type_filter,
            ) {
                continue;
            }
            if !Self::payload_matches_filter(sub, &event.payload) {
                continue;
            }
            if self
                .check_throttle_allowed(&sub.subscription.id, &sub.subscription.throttle)
                .await
            {
                matched.push(sub.subscription.clone());
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

    fn compile_filter(sub: &EventSubscription) -> Option<CompiledFilter> {
        let raw_filter = sub.filter.as_deref()?.trim();
        if raw_filter.is_empty() {
            return None;
        }

        match parse_filter(raw_filter) {
            Ok(expr) => Some(CompiledFilter::Parsed(expr)),
            Err(err) => {
                tracing::warn!(
                    subscription_id = %sub.id,
                    filter = raw_filter,
                    error = %err,
                    "Failed to parse event subscription filter; applying fail-open policy"
                );
                Some(CompiledFilter::Invalid)
            }
        }
    }

    fn payload_matches_filter(sub: &CompiledSubscription, payload: &Value) -> bool {
        match sub.compiled_filter.as_ref() {
            None => true,
            Some(CompiledFilter::Parsed(expr)) => evaluate_filter(expr, payload),
            Some(CompiledFilter::Invalid) => true,
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

/// Parse an event filter string into an `EventTypeFilter`.
///
/// Supported forms:
/// - `"all"` / `"*"` → `EventTypeFilter::All`
/// - `"category:AgentLifecycle"` → category filter
/// - `"AgentLifecycle.*"` → category filter
/// - `"TaskLifecycle.TaskFailed"` → exact filter (with category prefix)
/// - `"TaskFailed"` → exact filter
pub fn parse_event_type_filter(filter: &str) -> Option<EventTypeFilter> {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.eq_ignore_ascii_case("all") || trimmed == "*" {
        return Some(EventTypeFilter::All);
    }

    if let Some(cat_name) = trimmed.strip_prefix("category:") {
        let category = parse_event_category(cat_name.trim())?;
        return Some(EventTypeFilter::Category(category));
    }

    if let Some(category_name) = trimmed.strip_suffix(".*") {
        let category = parse_event_category(category_name.trim())?;
        return Some(EventTypeFilter::Category(category));
    }

    if let Some((category_name, event_name)) = trimmed.split_once('.') {
        if let Some(category) = parse_event_category(category_name.trim()) {
            let event_type = parse_event_type(event_name.trim())?;
            if event_type.category() != category {
                return None;
            }
            return Some(EventTypeFilter::Exact(event_type));
        }
    }

    let event_type = parse_event_type(trimmed)?;
    Some(EventTypeFilter::Exact(event_type))
}

pub fn parse_subscription_priority(priority: Option<&str>) -> Option<SubscriptionPriority> {
    match priority.map(str::trim).filter(|s| !s.is_empty()) {
        None => Some(SubscriptionPriority::Normal),
        Some(p) if p.eq_ignore_ascii_case("normal") => Some(SubscriptionPriority::Normal),
        Some(p) if p.eq_ignore_ascii_case("critical") => Some(SubscriptionPriority::Critical),
        Some(p) if p.eq_ignore_ascii_case("high") => Some(SubscriptionPriority::High),
        Some(p) if p.eq_ignore_ascii_case("low") => Some(SubscriptionPriority::Low),
        Some(_) => None,
    }
}

/// Default event subscriptions for a role.
pub fn default_subscriptions_for_role(role: &str) -> Vec<(EventTypeFilter, SubscriptionPriority)> {
    match role.trim().to_ascii_lowercase().as_str() {
        "orchestrator" => vec![
            (
                EventTypeFilter::Category(EventCategory::AgentLifecycle),
                SubscriptionPriority::High,
            ),
            (
                EventTypeFilter::Category(EventCategory::TaskLifecycle),
                SubscriptionPriority::High,
            ),
            (
                EventTypeFilter::Category(EventCategory::AgentCommunication),
                SubscriptionPriority::Normal,
            ),
        ],
        "security-monitor" => vec![
            (
                EventTypeFilter::Category(EventCategory::SecurityEvents),
                SubscriptionPriority::Critical,
            ),
            (
                EventTypeFilter::Exact(EventType::ToolSandboxViolation),
                SubscriptionPriority::Critical,
            ),
            (
                EventTypeFilter::Exact(EventType::ToolChecksumMismatch),
                SubscriptionPriority::Critical,
            ),
        ],
        "sysops" => vec![
            (
                EventTypeFilter::Category(EventCategory::SystemHealth),
                SubscriptionPriority::High,
            ),
            (
                EventTypeFilter::Category(EventCategory::HardwareEvents),
                SubscriptionPriority::Normal,
            ),
            (
                EventTypeFilter::Exact(EventType::ScheduledTaskFailed),
                SubscriptionPriority::High,
            ),
        ],
        "memory-manager" => vec![(
            EventTypeFilter::Category(EventCategory::MemoryEvents),
            SubscriptionPriority::High,
        )],
        "tool-manager" => vec![(
            EventTypeFilter::Category(EventCategory::ToolEvents),
            SubscriptionPriority::Normal,
        )],
        _ => vec![
            (
                EventTypeFilter::Exact(EventType::AgentAdded),
                SubscriptionPriority::Normal,
            ),
            (
                EventTypeFilter::Exact(EventType::DirectMessageReceived),
                SubscriptionPriority::Normal,
            ),
            (
                EventTypeFilter::Exact(EventType::DelegationReceived),
                SubscriptionPriority::Normal,
            ),
        ],
    }
}

pub fn parse_event_category(name: &str) -> Option<EventCategory> {
    match name {
        "AgentLifecycle" => Some(EventCategory::AgentLifecycle),
        "TaskLifecycle" => Some(EventCategory::TaskLifecycle),
        "SecurityEvents" => Some(EventCategory::SecurityEvents),
        "MemoryEvents" => Some(EventCategory::MemoryEvents),
        "SystemHealth" => Some(EventCategory::SystemHealth),
        "HardwareEvents" => Some(EventCategory::HardwareEvents),
        "ToolEvents" => Some(EventCategory::ToolEvents),
        "AgentCommunication" => Some(EventCategory::AgentCommunication),
        "ScheduleEvents" => Some(EventCategory::ScheduleEvents),
        "ExternalEvents" => Some(EventCategory::ExternalEvents),
        _ => None,
    }
}

pub fn parse_event_type(name: &str) -> Option<EventType> {
    match name {
        // AgentLifecycle
        "AgentAdded" => Some(EventType::AgentAdded),
        "AgentRemoved" => Some(EventType::AgentRemoved),
        "AgentPermissionGranted" => Some(EventType::AgentPermissionGranted),
        "AgentPermissionRevoked" => Some(EventType::AgentPermissionRevoked),
        // TaskLifecycle
        "TaskStarted" => Some(EventType::TaskStarted),
        "TaskCompleted" => Some(EventType::TaskCompleted),
        "TaskFailed" => Some(EventType::TaskFailed),
        "TaskTimedOut" => Some(EventType::TaskTimedOut),
        "TaskDelegated" => Some(EventType::TaskDelegated),
        "TaskRetrying" => Some(EventType::TaskRetrying),
        "TaskDeadlockDetected" => Some(EventType::TaskDeadlockDetected),
        "TaskPreempted" => Some(EventType::TaskPreempted),
        // SecurityEvents
        "PromptInjectionAttempt" => Some(EventType::PromptInjectionAttempt),
        "CapabilityViolation" => Some(EventType::CapabilityViolation),
        "UnauthorizedToolAccess" => Some(EventType::UnauthorizedToolAccess),
        "SecretsAccessAttempt" => Some(EventType::SecretsAccessAttempt),
        "SandboxEscapeAttempt" => Some(EventType::SandboxEscapeAttempt),
        "AuditLogTamperAttempt" => Some(EventType::AuditLogTamperAttempt),
        "AgentImpersonationAttempt" => Some(EventType::AgentImpersonationAttempt),
        "UnverifiedToolInstalled" => Some(EventType::UnverifiedToolInstalled),
        // MemoryEvents
        "ContextWindowNearLimit" => Some(EventType::ContextWindowNearLimit),
        "ContextWindowExhausted" => Some(EventType::ContextWindowExhausted),
        "EpisodicMemoryWritten" => Some(EventType::EpisodicMemoryWritten),
        "SemanticMemoryConflict" => Some(EventType::SemanticMemoryConflict),
        "MemorySearchFailed" => Some(EventType::MemorySearchFailed),
        "WorkingMemoryEviction" => Some(EventType::WorkingMemoryEviction),
        // SystemHealth
        "CPUSpikeDetected" => Some(EventType::CPUSpikeDetected),
        "MemoryPressure" => Some(EventType::MemoryPressure),
        "DiskSpaceLow" => Some(EventType::DiskSpaceLow),
        "DiskSpaceCritical" => Some(EventType::DiskSpaceCritical),
        "ProcessCrashed" => Some(EventType::ProcessCrashed),
        "NetworkInterfaceDown" => Some(EventType::NetworkInterfaceDown),
        "ContainerResourceQuotaExceeded" => Some(EventType::ContainerResourceQuotaExceeded),
        "KernelSubsystemError" => Some(EventType::KernelSubsystemError),
        // HardwareEvents
        "GPUAvailable" => Some(EventType::GPUAvailable),
        "GPUMemoryPressure" => Some(EventType::GPUMemoryPressure),
        "SensorReadingThresholdExceeded" => Some(EventType::SensorReadingThresholdExceeded),
        "DeviceConnected" => Some(EventType::DeviceConnected),
        "DeviceDisconnected" => Some(EventType::DeviceDisconnected),
        "HardwareAccessGranted" => Some(EventType::HardwareAccessGranted),
        // ToolEvents
        "ToolInstalled" => Some(EventType::ToolInstalled),
        "ToolRemoved" => Some(EventType::ToolRemoved),
        "ToolExecutionFailed" => Some(EventType::ToolExecutionFailed),
        "ToolSandboxViolation" => Some(EventType::ToolSandboxViolation),
        "ToolResourceQuotaExceeded" => Some(EventType::ToolResourceQuotaExceeded),
        "ToolChecksumMismatch" => Some(EventType::ToolChecksumMismatch),
        "ToolRegistryUpdated" => Some(EventType::ToolRegistryUpdated),
        // AgentCommunication
        "DirectMessageReceived" => Some(EventType::DirectMessageReceived),
        "BroadcastReceived" => Some(EventType::BroadcastReceived),
        "DelegationReceived" => Some(EventType::DelegationReceived),
        "DelegationResponseReceived" => Some(EventType::DelegationResponseReceived),
        "MessageDeliveryFailed" => Some(EventType::MessageDeliveryFailed),
        "AgentUnreachable" => Some(EventType::AgentUnreachable),
        // ScheduleEvents
        "CronJobFired" => Some(EventType::CronJobFired),
        "ScheduledTaskMissed" => Some(EventType::ScheduledTaskMissed),
        "ScheduledTaskCompleted" => Some(EventType::ScheduledTaskCompleted),
        "ScheduledTaskFailed" => Some(EventType::ScheduledTaskFailed),
        // ExternalEvents
        "WebhookReceived" => Some(EventType::WebhookReceived),
        "ExternalFileChanged" => Some(EventType::ExternalFileChanged),
        "ExternalAPIEvent" => Some(EventType::ExternalAPIEvent),
        "ExternalAlertReceived" => Some(EventType::ExternalAlertReceived),
        _ => None,
    }
}

pub fn parse_filter(filter_str: &str) -> Result<EventFilterExpr, String> {
    let clauses = split_filter_clauses(filter_str)?;
    let predicates = clauses
        .iter()
        .map(|clause| parse_predicate(clause))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(EventFilterExpr { predicates })
}

pub fn evaluate_filter(filter: &EventFilterExpr, payload: &Value) -> bool {
    filter
        .predicates
        .iter()
        .all(|pred| evaluate_predicate(pred, payload))
}

fn split_filter_clauses(filter_str: &str) -> Result<Vec<String>, String> {
    let trimmed = filter_str.trim();
    if trimmed.is_empty() {
        return Err("filter expression is empty".to_string());
    }

    let chars: Vec<(usize, char)> = trimmed.char_indices().collect();
    let mut clauses = Vec::new();
    let mut clause_start = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape_next = false;
    let mut list_depth = 0usize;
    let mut i = 0usize;

    while i < chars.len() {
        let (_, ch) = chars[i];
        if (in_single || in_double) && escape_next {
            escape_next = false;
            i += 1;
            continue;
        }

        if (in_single || in_double) && ch == '\\' {
            escape_next = true;
            i += 1;
            continue;
        }

        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '[' if !in_single && !in_double => list_depth += 1,
            ']' if !in_single && !in_double => {
                if list_depth == 0 {
                    return Err("unexpected closing bracket in filter expression".to_string());
                }
                list_depth -= 1;
            }
            _ => {}
        }

        if !in_single
            && !in_double
            && list_depth == 0
            && i + 2 < chars.len()
            && chars[i].1.eq_ignore_ascii_case(&'a')
            && chars[i + 1].1.eq_ignore_ascii_case(&'n')
            && chars[i + 2].1.eq_ignore_ascii_case(&'d')
        {
            let prev_is_ws = i == 0 || chars[i - 1].1.is_whitespace();
            let next_is_ws = i + 3 == chars.len() || chars[i + 3].1.is_whitespace();
            if prev_is_ws && next_is_ws {
                let clause_end = chars[i].0;
                let clause = trimmed[clause_start..clause_end].trim();
                if clause.is_empty() {
                    return Err("filter expression contains an empty predicate".to_string());
                }
                clauses.push(clause.to_string());
                clause_start = if i + 3 < chars.len() {
                    chars[i + 3].0
                } else {
                    trimmed.len()
                };
                i += 3;
                continue;
            }
        }

        i += 1;
    }

    if in_single || in_double {
        return Err("unterminated quoted string in filter expression".to_string());
    }
    if escape_next {
        return Err("dangling escape in quoted filter expression".to_string());
    }
    if list_depth != 0 {
        return Err("unterminated list literal in filter expression".to_string());
    }

    let tail = trimmed[clause_start..].trim();
    if tail.is_empty() {
        return Err("filter expression contains an empty predicate".to_string());
    }
    clauses.push(tail.to_string());

    Ok(clauses)
}

fn parse_predicate(clause: &str) -> Result<FilterPredicate, String> {
    let captures = predicate_regex()
        .captures(clause)
        .ok_or_else(|| format!("invalid predicate syntax: '{}'", clause))?;

    let field = captures
        .get(1)
        .map(|m| m.as_str().trim().to_string())
        .ok_or_else(|| format!("missing field in predicate '{}'", clause))?;

    let op = captures
        .get(2)
        .ok_or_else(|| format!("missing operator in predicate '{}'", clause))
        .and_then(|m| parse_operator(m.as_str()))?;

    let value_raw = captures
        .get(3)
        .map(|m| m.as_str())
        .ok_or_else(|| format!("missing value in predicate '{}'", clause))?;

    let value = parse_value_for_op(op, value_raw)?;

    Ok(FilterPredicate { field, op, value })
}

fn parse_operator(op: &str) -> Result<FilterOp, String> {
    match op.to_ascii_uppercase().as_str() {
        "==" => Ok(FilterOp::Eq),
        "!=" => Ok(FilterOp::NotEq),
        ">" => Ok(FilterOp::Gt),
        ">=" => Ok(FilterOp::Gte),
        "<" => Ok(FilterOp::Lt),
        "<=" => Ok(FilterOp::Lte),
        "IN" => Ok(FilterOp::In),
        "CONTAINS" => Ok(FilterOp::Contains),
        other => Err(format!("unsupported operator '{}'", other)),
    }
}

fn parse_value_for_op(op: FilterOp, raw_value: &str) -> Result<FilterValue, String> {
    match op {
        FilterOp::In => Ok(FilterValue::List(parse_list_value(raw_value)?)),
        FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte => {
            match parse_scalar(raw_value)? {
                FilterValue::Number(n) => Ok(FilterValue::Number(n)),
                _ => Err("numeric comparisons require a number value".to_string()),
            }
        }
        FilterOp::Contains => match parse_scalar(raw_value)? {
            FilterValue::String(s) => Ok(FilterValue::String(s)),
            _ => Err("CONTAINS requires a string value".to_string()),
        },
        FilterOp::Eq | FilterOp::NotEq => parse_scalar(raw_value),
    }
}

fn parse_scalar(raw_value: &str) -> Result<FilterValue, String> {
    let value = raw_value.trim();
    if value.is_empty() {
        return Err("predicate value cannot be empty".to_string());
    }

    if value.starts_with('\'') || value.starts_with('"') {
        let s = unquote(value)
            .ok_or_else(|| "invalid quoted string literal in predicate value".to_string())?;
        return Ok(FilterValue::String(s));
    }

    if value.eq_ignore_ascii_case("true") {
        return Ok(FilterValue::Bool(true));
    }
    if value.eq_ignore_ascii_case("false") {
        return Ok(FilterValue::Bool(false));
    }

    if let Ok(n) = value.parse::<f64>() {
        return Ok(FilterValue::Number(n));
    }

    if value.starts_with('[') && value.ends_with(']') {
        return Ok(FilterValue::List(parse_list_value(value)?));
    }

    Ok(FilterValue::String(value.to_string()))
}

fn parse_list_value(raw_value: &str) -> Result<Vec<String>, String> {
    let trimmed = raw_value.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err("IN requires a list literal like ['a', 'b']".to_string());
    }

    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }

    let items = split_list_items(inner)?;
    if items.iter().any(|item| item.trim().is_empty()) {
        return Err("list literal contains an empty item".to_string());
    }

    let values = items
        .into_iter()
        .map(|item| {
            let token = item.trim();
            if token.starts_with('\'') || token.starts_with('"') {
                return unquote(token)
                    .ok_or_else(|| "invalid quoted string literal in list item".to_string());
            }
            Ok(token.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(values)
}

fn split_list_items(inner: &str) -> Result<Vec<String>, String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape_next = false;

    for ch in inner.chars() {
        if (in_single || in_double) && escape_next {
            current.push(ch);
            escape_next = false;
            continue;
        }

        if (in_single || in_double) && ch == '\\' {
            current.push(ch);
            escape_next = true;
            continue;
        }

        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                current.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                current.push(ch);
            }
            ',' if !in_single && !in_double => {
                items.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if in_single || in_double {
        return Err("unterminated quoted string in list literal".to_string());
    }
    if escape_next {
        return Err("dangling escape in quoted list literal".to_string());
    }

    items.push(current.trim().to_string());
    Ok(items)
}

fn unquote(value: &str) -> Option<String> {
    if value.len() < 2 {
        return None;
    }

    let mut chars = value.chars();
    let quote = chars.next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }

    let mut out = String::new();
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if escaped {
            match ch {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                '\'' => out.push('\''),
                '"' => out.push('"'),
                other => out.push(other),
            }
            escaped = false;
            continue;
        }

        if ch == '\\' {
            escaped = true;
            continue;
        }

        if ch == quote {
            if chars.as_str().is_empty() {
                return Some(out);
            }
            return None;
        }

        out.push(ch);
    }

    None
}

/// Evaluate a single predicate against a JSON payload.
///
/// **Missing-field semantics:** if the field path does not exist in the payload,
/// the predicate returns `false` for *all* operators — including `!=`. This is
/// intentional (fail-closed on absence) and consistent with SQL `NULL` handling,
/// but it means `status != "failed"` will *not* match events that lack a `status`
/// field entirely.
fn evaluate_predicate(pred: &FilterPredicate, payload: &Value) -> bool {
    let field_value = match get_json_field(payload, &pred.field) {
        Some(value) => value,
        None => return false,
    };

    match pred.op {
        FilterOp::Eq => compare_eq(field_value, &pred.value).unwrap_or(false),
        FilterOp::NotEq => compare_eq(field_value, &pred.value)
            .map(|is_equal| !is_equal)
            .unwrap_or(false),
        FilterOp::Gt => compare_number(field_value, &pred.value, |lhs, rhs| lhs > rhs),
        FilterOp::Gte => compare_number(field_value, &pred.value, |lhs, rhs| lhs >= rhs),
        FilterOp::Lt => compare_number(field_value, &pred.value, |lhs, rhs| lhs < rhs),
        FilterOp::Lte => compare_number(field_value, &pred.value, |lhs, rhs| lhs <= rhs),
        FilterOp::In => match (&pred.value, field_value) {
            (FilterValue::List(values), Value::String(actual)) => {
                values.iter().any(|v| v == actual)
            }
            // When the field is a JSON array, the predicate passes if *any* element
            // of the field array appears in the filter list ("intersection non-empty"
            // semantics, not "all elements contained").
            (FilterValue::List(values), Value::Array(actual_values)) => actual_values
                .iter()
                .filter_map(|v| v.as_str())
                .any(|actual| values.iter().any(|v| v == actual)),
            _ => false,
        },
        FilterOp::Contains => match (&pred.value, field_value) {
            (FilterValue::String(needle), Value::String(haystack)) => haystack.contains(needle),
            _ => false,
        },
    }
}

fn compare_eq(field_value: &Value, expected: &FilterValue) -> Option<bool> {
    match expected {
        FilterValue::String(expected_str) => field_value.as_str().map(|v| v == expected_str),
        // Note: uses exact IEEE 754 equality. Reliable for integers and simple
        // decimals parsed by serde_json, but `price == 19.99` may behave
        // unexpectedly for values derived from arithmetic. Prefer integer thresholds
        // (e.g. `cpu_percent == 90`) for `==`/`!=` on numbers.
        FilterValue::Number(expected_num) => value_as_f64(field_value).map(|v| v == *expected_num),
        FilterValue::Bool(expected_bool) => field_value.as_bool().map(|v| v == *expected_bool),
        FilterValue::List(expected_items) => {
            let actual_items = field_value.as_array()?;
            let mut actual_strings = Vec::with_capacity(actual_items.len());
            for item in actual_items {
                actual_strings.push(item.as_str()?.to_string());
            }
            Some(actual_strings == *expected_items)
        }
    }
}

fn compare_number(
    field_value: &Value,
    expected: &FilterValue,
    comparator: impl Fn(f64, f64) -> bool,
) -> bool {
    let field_number = match value_as_f64(field_value) {
        Some(n) => n,
        None => return false,
    };
    let expected_number = match expected {
        FilterValue::Number(n) => *n,
        _ => return false,
    };
    comparator(field_number, expected_number)
}

/// Extract a numeric value from a JSON value, coercing JSON strings that look
/// like numbers (e.g. `"90"` → `90.0`). This allows filters to match payloads
/// where a numeric field was serialised as a string, but it can mask payload
/// type bugs. Only scalar JSON strings are coerced; arrays and objects return `None`.
fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

pub fn get_json_field<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Some(value);
    }

    let mut current = value;
    for key in trimmed.split('.') {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        current = current.get(key)?;
    }
    Some(current)
}

fn predicate_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*([A-Za-z0-9_.-]+)\s*(==|!=|>=|<=|>|<|(?i:IN)|(?i:CONTAINS))\s*(.+?)\s*$")
            .expect("predicate regex must compile")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn make_subscription(
        agent_id: AgentID,
        filter: EventTypeFilter,
        throttle: ThrottlePolicy,
    ) -> EventSubscription {
        make_subscription_with_payload_filter(agent_id, filter, throttle, None)
    }

    fn make_subscription_with_payload_filter(
        agent_id: AgentID,
        filter: EventTypeFilter,
        throttle: ThrottlePolicy,
        payload_filter: Option<&str>,
    ) -> EventSubscription {
        EventSubscription {
            id: SubscriptionID::new(),
            agent_id,
            event_type_filter: filter,
            filter: payload_filter.map(|s| s.to_string()),
            priority: SubscriptionPriority::Normal,
            throttle,
            enabled: true,
            created_at: chrono::Utc::now(),
        }
    }

    fn make_event(event_type: EventType) -> EventMessage {
        make_event_with_payload(event_type, json!({}))
    }

    fn make_event_with_payload(event_type: EventType, payload: Value) -> EventMessage {
        EventMessage {
            id: EventID::new(),
            event_type,
            source: EventSource::AgentLifecycle,
            payload,
            severity: EventSeverity::Info,
            timestamp: chrono::Utc::now(),
            signature: vec![],
            trace_id: TraceID::new(),
            chain_depth: 0,
        }
    }

    #[test]
    fn test_parse_event_type_filter_accepts_category_wildcard() {
        assert_eq!(
            parse_event_type_filter("SecurityEvents.*"),
            Some(EventTypeFilter::Category(EventCategory::SecurityEvents))
        );
    }

    #[test]
    fn test_parse_event_type_filter_accepts_category_dot_event() {
        assert_eq!(
            parse_event_type_filter("TaskLifecycle.TaskFailed"),
            Some(EventTypeFilter::Exact(EventType::TaskFailed))
        );
    }

    #[test]
    fn test_parse_event_type_filter_rejects_category_event_mismatch() {
        assert_eq!(parse_event_type_filter("TaskLifecycle.AgentAdded"), None);
    }

    #[test]
    fn test_default_subscriptions_for_orchestrator() {
        let defaults = default_subscriptions_for_role("orchestrator");
        assert!(defaults.contains(&(
            EventTypeFilter::Category(EventCategory::AgentLifecycle),
            SubscriptionPriority::High
        )));
        assert!(defaults.contains(&(
            EventTypeFilter::Category(EventCategory::TaskLifecycle),
            SubscriptionPriority::High
        )));
        assert!(defaults.contains(&(
            EventTypeFilter::Category(EventCategory::AgentCommunication),
            SubscriptionPriority::Normal
        )));
    }

    #[test]
    fn test_parse_filter_simple_number_clause() {
        let expr = parse_filter("cpu_percent > 85").expect("must parse");
        assert_eq!(expr.predicates.len(), 1);
        assert_eq!(
            expr.predicates[0],
            FilterPredicate {
                field: "cpu_percent".to_string(),
                op: FilterOp::Gt,
                value: FilterValue::Number(85.0),
            }
        );
    }

    #[test]
    fn test_parse_filter_simple_string_clause() {
        let expr = parse_filter("severity == Critical").expect("must parse");
        assert_eq!(expr.predicates.len(), 1);
        assert_eq!(
            expr.predicates[0],
            FilterPredicate {
                field: "severity".to_string(),
                op: FilterOp::Eq,
                value: FilterValue::String("Critical".to_string()),
            }
        );
    }

    #[test]
    fn test_parse_filter_in_list_clause() {
        let expr =
            parse_filter("tool_id IN ['http-client', 'shell-exec']").expect("must parse list");
        assert_eq!(expr.predicates.len(), 1);
        assert_eq!(
            expr.predicates[0],
            FilterPredicate {
                field: "tool_id".to_string(),
                op: FilterOp::In,
                value: FilterValue::List(
                    vec!["http-client".to_string(), "shell-exec".to_string(),]
                ),
            }
        );
    }

    #[test]
    fn test_parse_filter_and_clause() {
        let expr = parse_filter("cpu_percent > 85 AND severity == Critical").expect("must parse");
        assert_eq!(expr.predicates.len(), 2);
        assert_eq!(expr.predicates[0].field, "cpu_percent");
        assert_eq!(expr.predicates[1].field, "severity");
    }

    #[test]
    fn test_parse_filter_and_inside_string_literal() {
        let expr = parse_filter(r#"message CONTAINS "CPU AND Memory""#).expect("must parse");
        assert_eq!(expr.predicates.len(), 1);
        assert_eq!(
            expr.predicates[0].value,
            FilterValue::String("CPU AND Memory".to_string())
        );
    }

    #[test]
    fn test_parse_filter_and_inside_list_literal() {
        let expr =
            parse_filter("tool_id IN ['http AND client', 'shell-exec']").expect("must parse");
        assert_eq!(expr.predicates.len(), 1);
        assert_eq!(
            expr.predicates[0].value,
            FilterValue::List(vec![
                "http AND client".to_string(),
                "shell-exec".to_string(),
            ])
        );
    }

    #[test]
    fn test_parse_filter_contains_with_escaped_quotes() {
        let expr = parse_filter(r#"message CONTAINS "CPU \"AND\" Memory""#).expect("must parse");
        assert_eq!(expr.predicates.len(), 1);
        assert_eq!(
            expr.predicates[0].value,
            FilterValue::String(r#"CPU "AND" Memory"#.to_string())
        );
    }

    #[test]
    fn test_parse_filter_in_list_with_escaped_quote() {
        let expr =
            parse_filter(r#"tool_id IN ['shell\'exec', "http-client"]"#).expect("must parse");
        assert_eq!(expr.predicates.len(), 1);
        assert_eq!(
            expr.predicates[0].value,
            FilterValue::List(vec!["shell'exec".to_string(), "http-client".to_string()])
        );
    }

    #[test]
    fn test_parse_filter_rejects_dangling_escape() {
        let err = parse_filter(r#"message CONTAINS "oops\""#)
            .expect_err("must reject dangling escape in quote");
        assert!(err.contains("quoted") || err.contains("escape"));
    }

    #[test]
    fn test_evaluate_filter_number_true() {
        let filter = parse_filter("cpu_percent > 85").expect("must parse");
        let payload = json!({ "cpu_percent": 90 });
        assert!(evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_number_false() {
        let filter = parse_filter("cpu_percent > 85").expect("must parse");
        let payload = json!({ "cpu_percent": 70 });
        assert!(!evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_string_true() {
        let filter = parse_filter("severity == Critical").expect("must parse");
        let payload = json!({ "severity": "Critical" });
        assert!(evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_in_list_true() {
        let filter = parse_filter("tool_id IN ['http-client', 'shell-exec']").expect("must parse");
        let payload = json!({ "tool_id": "http-client" });
        assert!(evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_missing_field_is_false() {
        let filter = parse_filter("cpu_percent > 85").expect("must parse");
        let payload = json!({ "mem_percent": 90 });
        assert!(!evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_nested_path() {
        let filter = parse_filter("task.metrics.cpu_percent >= 80").expect("must parse");
        let payload = json!({
            "task": {
                "metrics": {
                    "cpu_percent": 81
                }
            }
        });
        assert!(evaluate_filter(&filter, &payload));
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

    #[tokio::test]
    async fn test_subscription_payload_filter_is_applied() {
        let bus = EventBus::new();
        let sub = make_subscription_with_payload_filter(
            AgentID::new(),
            EventTypeFilter::Exact(EventType::CPUSpikeDetected),
            ThrottlePolicy::None,
            Some("cpu_percent > 85"),
        );
        bus.subscribe(sub).await;

        let matching_event =
            make_event_with_payload(EventType::CPUSpikeDetected, json!({ "cpu_percent": 90 }));
        let non_matching_event =
            make_event_with_payload(EventType::CPUSpikeDetected, json!({ "cpu_percent": 70 }));

        assert_eq!(bus.evaluate_subscriptions(&matching_event).await.len(), 1);
        assert_eq!(
            bus.evaluate_subscriptions(&non_matching_event).await.len(),
            0
        );
    }

    #[tokio::test]
    async fn test_invalid_payload_filter_fails_open() {
        let bus = EventBus::new();
        let sub = make_subscription_with_payload_filter(
            AgentID::new(),
            EventTypeFilter::Exact(EventType::CPUSpikeDetected),
            ThrottlePolicy::None,
            Some("cpu_percent >>> 85"),
        );
        bus.subscribe(sub).await;

        let event =
            make_event_with_payload(EventType::CPUSpikeDetected, json!({ "cpu_percent": 70 }));
        assert_eq!(bus.evaluate_subscriptions(&event).await.len(), 1);
    }

    // ── Missing-operator tests added after Phase 07 review ────────────

    #[test]
    fn test_evaluate_filter_contains_true() {
        let filter = parse_filter(r#"message CONTAINS "error""#).expect("must parse");
        let payload = json!({ "message": "An error occurred" });
        assert!(evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_contains_false() {
        let filter = parse_filter(r#"message CONTAINS "error""#).expect("must parse");
        let payload = json!({ "message": "All systems normal" });
        assert!(!evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_not_eq_true() {
        let filter = parse_filter("status != failed").expect("must parse");
        let payload = json!({ "status": "ok" });
        assert!(evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_not_eq_false() {
        let filter = parse_filter("status != failed").expect("must parse");
        let payload = json!({ "status": "failed" });
        assert!(!evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_not_eq_missing_field_is_false() {
        // Documents the missing-field-is-false behaviour for != specifically:
        // an event without a `status` field does NOT match `status != "failed"`.
        let filter = parse_filter("status != failed").expect("must parse");
        let payload = json!({ "cpu_percent": 50 });
        assert!(!evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_bool_eq_true() {
        let filter = parse_filter("enabled == true").expect("must parse");
        let payload = json!({ "enabled": true });
        assert!(evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_bool_eq_false() {
        let filter = parse_filter("enabled == true").expect("must parse");
        let payload = json!({ "enabled": false });
        assert!(!evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_in_with_json_array_any_match() {
        // Field is a JSON array; predicate passes when any element is in the filter list.
        let filter = parse_filter("tags IN ['critical', 'high']").expect("must parse");
        let payload = json!({ "tags": ["low", "critical"] });
        assert!(evaluate_filter(&filter, &payload));
    }

    #[test]
    fn test_evaluate_filter_in_with_json_array_no_match() {
        let filter = parse_filter("tags IN ['critical', 'high']").expect("must parse");
        let payload = json!({ "tags": ["low", "medium"] });
        assert!(!evaluate_filter(&filter, &payload));
    }
}
