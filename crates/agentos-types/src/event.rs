use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ── Event Categories ──────────────────────────────────────────────

/// Top-level category grouping related event types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventCategory {
    AgentLifecycle,
    TaskLifecycle,
    SecurityEvents,
    MemoryEvents,
    SystemHealth,
    HardwareEvents,
    ToolEvents,
    AgentCommunication,
    ScheduleEvents,
    ExternalEvents,
}

// ── Event Types ───────────────────────────────────────────────────

/// Every discrete event the OS can emit. Non-exhaustive: new variants
/// are added as phases are implemented.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    // ── AgentLifecycle (Phase 1) ──
    AgentAdded,
    AgentRemoved,
    AgentPermissionGranted,
    AgentPermissionRevoked,

    // ── TaskLifecycle (Phase 2) ──
    TaskStarted,
    TaskCompleted,
    TaskFailed,
    TaskTimedOut,
    TaskDelegated,
    TaskRetrying,
    TaskDeadlockDetected,
    TaskPreempted,

    // ── SecurityEvents (Phase 2) ──
    PromptInjectionAttempt,
    CapabilityViolation,
    UnauthorizedToolAccess,
    SecretsAccessAttempt,
    SandboxEscapeAttempt,
    AuditLogTamperAttempt,
    AgentImpersonationAttempt,
    UnverifiedToolInstalled,

    // ── MemoryEvents (Phase 2) ──
    ContextWindowNearLimit,
    ContextWindowExhausted,
    EpisodicMemoryWritten,
    SemanticMemoryConflict,
    MemorySearchFailed,
    WorkingMemoryEviction,

    // ── SystemHealth (Phase 3) ──
    CPUSpikeDetected,
    MemoryPressure,
    DiskSpaceLow,
    DiskSpaceCritical,
    ProcessCrashed,
    NetworkInterfaceDown,
    ContainerResourceQuotaExceeded,
    KernelSubsystemError,

    // ── HardwareEvents (Phase 3) ──
    GPUAvailable,
    GPUMemoryPressure,
    SensorReadingThresholdExceeded,
    DeviceConnected,
    DeviceDisconnected,
    HardwareAccessGranted,

    // ── ToolEvents (Phase 5) ──
    ToolInstalled,
    ToolRemoved,
    ToolExecutionFailed,
    ToolSandboxViolation,
    ToolResourceQuotaExceeded,
    ToolChecksumMismatch,
    ToolRegistryUpdated,

    // ── AgentCommunication (Phase 4) ──
    DirectMessageReceived,
    BroadcastReceived,
    DelegationReceived,
    DelegationResponseReceived,
    MessageDeliveryFailed,
    AgentUnreachable,

    // ── ScheduleEvents (Phase 4) ──
    CronJobFired,
    ScheduledTaskMissed,
    ScheduledTaskCompleted,
    ScheduledTaskFailed,

    // ── ExternalEvents (Phase 5) ──
    WebhookReceived,
    ExternalFileChanged,
    ExternalAPIEvent,
    ExternalAlertReceived,
}

impl EventType {
    /// Return the category this event belongs to.
    pub fn category(&self) -> EventCategory {
        match self {
            Self::AgentAdded
            | Self::AgentRemoved
            | Self::AgentPermissionGranted
            | Self::AgentPermissionRevoked => EventCategory::AgentLifecycle,

            Self::TaskStarted
            | Self::TaskCompleted
            | Self::TaskFailed
            | Self::TaskTimedOut
            | Self::TaskDelegated
            | Self::TaskRetrying
            | Self::TaskDeadlockDetected
            | Self::TaskPreempted => EventCategory::TaskLifecycle,

            Self::PromptInjectionAttempt
            | Self::CapabilityViolation
            | Self::UnauthorizedToolAccess
            | Self::SecretsAccessAttempt
            | Self::SandboxEscapeAttempt
            | Self::AuditLogTamperAttempt
            | Self::AgentImpersonationAttempt
            | Self::UnverifiedToolInstalled => EventCategory::SecurityEvents,

            Self::ContextWindowNearLimit
            | Self::ContextWindowExhausted
            | Self::EpisodicMemoryWritten
            | Self::SemanticMemoryConflict
            | Self::MemorySearchFailed
            | Self::WorkingMemoryEviction => EventCategory::MemoryEvents,

            Self::CPUSpikeDetected
            | Self::MemoryPressure
            | Self::DiskSpaceLow
            | Self::DiskSpaceCritical
            | Self::ProcessCrashed
            | Self::NetworkInterfaceDown
            | Self::ContainerResourceQuotaExceeded
            | Self::KernelSubsystemError => EventCategory::SystemHealth,

            Self::GPUAvailable
            | Self::GPUMemoryPressure
            | Self::SensorReadingThresholdExceeded
            | Self::DeviceConnected
            | Self::DeviceDisconnected
            | Self::HardwareAccessGranted => EventCategory::HardwareEvents,

            Self::ToolInstalled
            | Self::ToolRemoved
            | Self::ToolExecutionFailed
            | Self::ToolSandboxViolation
            | Self::ToolResourceQuotaExceeded
            | Self::ToolChecksumMismatch
            | Self::ToolRegistryUpdated => EventCategory::ToolEvents,

            Self::DirectMessageReceived
            | Self::BroadcastReceived
            | Self::DelegationReceived
            | Self::DelegationResponseReceived
            | Self::MessageDeliveryFailed
            | Self::AgentUnreachable => EventCategory::AgentCommunication,

            Self::CronJobFired
            | Self::ScheduledTaskMissed
            | Self::ScheduledTaskCompleted
            | Self::ScheduledTaskFailed => EventCategory::ScheduleEvents,

            Self::WebhookReceived
            | Self::ExternalFileChanged
            | Self::ExternalAPIEvent
            | Self::ExternalAlertReceived => EventCategory::ExternalEvents,
        }
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::fmt::Display for EventCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

// ── Event Source ──────────────────────────────────────────────────

/// Which kernel subsystem emitted the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventSource {
    AgentLifecycle,
    InferenceKernel,
    TaskScheduler,
    SecurityEngine,
    MemoryArbiter,
    ToolRunner,
    HardwareAbstractionLayer,
    AgentMessageBus,
    ContextManager,
    SecretsVault,
    Scheduler,
    ExternalBridge,
}

// ── Event Severity ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventSeverity {
    /// Normal operation — agent may act but doesn't have to.
    Info,
    /// Something unusual — agent should investigate.
    Warning,
    /// Something is wrong — agent must respond.
    Critical,
}

// ── Event Message ─────────────────────────────────────────────────

/// A single event emitted by an OS subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMessage {
    pub id: EventID,
    pub event_type: EventType,
    pub source: EventSource,
    /// Structured payload data — typed payloads per event type come in later phases.
    pub payload: serde_json::Value,
    pub severity: EventSeverity,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// HMAC-SHA256 signature over the canonical event representation.
    #[serde(with = "hex_bytes")]
    pub signature: Vec<u8>,
    pub trace_id: TraceID,
    /// How many event→task→event hops preceded this one (for loop detection).
    pub chain_depth: u32,
}

// ── Subscription ──────────────────────────────────────────────────

/// Filter for which event types a subscription matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventTypeFilter {
    /// Subscribe to one specific event type.
    Exact(EventType),
    /// Subscribe to all events in a category.
    Category(EventCategory),
    /// Subscribe to everything (use carefully).
    All,
}

/// How urgently to deliver a triggered task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SubscriptionPriority {
    /// Deliver immediately, preempt other tasks if needed.
    Critical,
    /// Deliver in next scheduler slot.
    High,
    /// Deliver when agent is available.
    #[default]
    Normal,
    /// Deliver when system is idle.
    Low,
}

/// Rate-limiting policy for a subscription to prevent event floods.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThrottlePolicy {
    /// No throttling — deliver every occurrence.
    #[default]
    None,
    /// At most one delivery per given duration.
    MaxOncePerDuration(Duration),
    /// At most N deliveries per given duration.
    MaxCountPerDuration(u32, Duration),
}

/// An agent's subscription to a set of event types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSubscription {
    pub id: SubscriptionID,
    pub agent_id: AgentID,
    pub event_type_filter: EventTypeFilter,
    /// Optional filter predicate evaluated against the event payload (Phase 2+).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default)]
    pub priority: SubscriptionPriority,
    #[serde(default)]
    pub throttle: ThrottlePolicy,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// ── Hex serialization helper for Vec<u8> ──────────────────────────

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_category() {
        assert_eq!(
            EventType::AgentAdded.category(),
            EventCategory::AgentLifecycle
        );
        assert_eq!(
            EventType::TaskFailed.category(),
            EventCategory::TaskLifecycle
        );
        assert_eq!(
            EventType::CapabilityViolation.category(),
            EventCategory::SecurityEvents
        );
        assert_eq!(
            EventType::CPUSpikeDetected.category(),
            EventCategory::SystemHealth
        );
        assert_eq!(
            EventType::WebhookReceived.category(),
            EventCategory::ExternalEvents
        );
    }

    #[test]
    fn test_event_type_display() {
        assert_eq!(EventType::AgentAdded.to_string(), "AgentAdded");
    }

    #[test]
    fn test_throttle_default_is_none() {
        assert_eq!(ThrottlePolicy::default(), ThrottlePolicy::None);
    }

    #[test]
    fn test_event_message_serialization() {
        let msg = EventMessage {
            id: EventID::new(),
            event_type: EventType::AgentAdded,
            source: EventSource::AgentLifecycle,
            payload: serde_json::json!({"agent_name": "test"}),
            severity: EventSeverity::Info,
            timestamp: chrono::Utc::now(),
            signature: vec![0xAB, 0xCD],
            trace_id: TraceID::new(),
            chain_depth: 0,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deser: EventMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.event_type, EventType::AgentAdded);
        assert_eq!(deser.signature, vec![0xAB, 0xCD]);
    }
}
