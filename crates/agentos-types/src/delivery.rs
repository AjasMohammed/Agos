use crate::ChannelInstanceID;
use serde::{Deserialize, Serialize};

/// How a scheduled/background task result should reach someone after completion.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DeliveryMode {
    #[default]
    /// Persist the run record and do nothing else. Default when unspecified.
    Silent,

    /// Render the result as a notification and send it to `target` without
    /// re-involving the creator agent. Cheapest path.
    Direct {
        target: NotifyTarget,
        /// Optional subject override; otherwise derived from schedule name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        /// "info" | "warning" | "urgent" | "critical"
        #[serde(default = "default_priority")]
        priority: String,
    },

    /// Inject the result as a synthetic message into the creator agent's
    /// context, triggering one more inference turn. The agent decides what
    /// (if anything) to tell the user.
    ViaAgent {
        /// Which agent receives the result. `None` → use the creator agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<crate::AgentID>,
        /// Cap on re-triggered schedule depth — prevents self-scheduling loops.
        /// The kernel enforces a hard cap of 3 regardless of this value.
        #[serde(default = "default_depth_cap")]
        max_depth: u8,
    },
}

/// Where a `Direct` delivery sends its notification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifyTarget {
    /// User's notification inbox (uses the existing notify-user tool internally).
    UserInbox,
    /// A specific channel the creator agent has access to.
    Channel { id: ChannelInstanceID },
    /// File path (must be inside an allowed storage zone).
    File { path: String },
}

fn default_priority() -> String {
    "info".into()
}
fn default_depth_cap() -> u8 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_round_trips() {
        let v = DeliveryMode::Silent;
        let json = serde_json::to_string(&v).unwrap();
        let back: DeliveryMode = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    /// The serialized form of `Silent` is used as the SQL DEFAULT in the v2
    /// schema migration. If serde changes this output, the DEFAULT must be
    /// updated to match or existing rows will fail to deserialize.
    #[test]
    fn silent_serializes_to_exact_migration_default() {
        let s = serde_json::to_string(&DeliveryMode::Silent).unwrap();
        assert_eq!(
            s, r#"{"mode":"silent"}"#,
            "DeliveryMode::Silent serialization must match the v2 SQL migration DEFAULT"
        );
    }

    #[test]
    fn direct_round_trips() {
        let v = DeliveryMode::Direct {
            target: NotifyTarget::UserInbox,
            subject: Some("hello".into()),
            priority: "urgent".into(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: DeliveryMode = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn via_agent_round_trips() {
        let v = DeliveryMode::ViaAgent {
            agent_id: None,
            max_depth: 2,
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: DeliveryMode = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn channel_target_round_trips() {
        let id = ChannelInstanceID::new();
        let v = DeliveryMode::Direct {
            target: NotifyTarget::Channel { id },
            subject: None,
            priority: "info".into(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: DeliveryMode = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }
}
