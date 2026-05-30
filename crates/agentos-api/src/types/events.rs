//! Event subscription + emission DTOs (Phase 05).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// An agent's event subscription.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiEventSubscription {
    /// Subscription id (UUID).
    pub id: String,
    /// Owning agent id (UUID).
    pub agent_id: String,
    /// Event-type filter (debug-rendered, e.g. `All`, `Category(...)`, `Exact(...)`).
    pub event_type_filter: String,
    /// Optional payload filter predicate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_filter: Option<String>,
    /// Delivery priority (`Critical` | `High` | `Normal` | `Low`).
    pub priority: String,
    /// Throttle policy (debug-rendered).
    pub throttle: String,
    /// Whether the subscription is active.
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

/// Create a new event subscription. Mirrors the kernel `event subscribe` command.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateSubscriptionRequest {
    /// Name of the subscribing agent.
    pub agent_name: String,
    /// Event filter: `all`, `category:<name>`, or an exact event type (`AgentAdded`).
    pub event_filter: String,
    /// Optional payload filter predicate.
    #[serde(default)]
    pub payload_filter: Option<String>,
    /// Optional throttle: `none`, `once_per:<dur>`, or `max:<count>/<dur>`.
    #[serde(default)]
    pub throttle: Option<String>,
    /// Optional priority: `critical` | `high` | `normal` | `low`.
    #[serde(default)]
    pub priority: Option<String>,
}

/// Emit an event into the kernel event bus.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct EmitEventRequest {
    /// Exact event type name (e.g. `TaskCompleted`).
    pub event_type: String,
    /// Severity: `info` | `warning` | `critical` (default `info`).
    #[serde(default)]
    pub severity: Option<String>,
    /// Arbitrary JSON payload.
    #[serde(default)]
    pub payload: serde_json::Value,
}
