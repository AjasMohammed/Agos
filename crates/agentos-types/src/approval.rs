//! User-selectable approval modes for tool execution.
//!
//! Modes decide the **default** behaviour when a tool call's risk class would
//! normally require approval. Specific [`AutoApproveRule`]s and learned
//! "allow always" entries can still lift a prompt → allow, but cannot lift
//! a `ControlPlane` operation; `ControlPlane` is the non-overridable floor
//! and always prompts (or denies, under `Deny`).

use serde::{Deserialize, Serialize};

/// Coarse user-selectable approval policy applied per agent (or globally).
///
/// Mode-vs-risk-class decision matrix:
///
/// | Mode        | ReadonlyScoped | ReadonlyExternal | WriteScoped | ExecCapable | ControlPlane | Interactive |
/// |-------------|:--------------:|:----------------:|:-----------:|:-----------:|:------------:|:-----------:|
/// | `Auto`      | allow          | allow            | allow       | allow       | **prompt**   | allow       |
/// | `AskEdit`   | allow          | allow            | prompt      | prompt      | prompt       | prompt      |
/// | `AskAlways` | allow          | prompt           | prompt      | prompt      | prompt       | prompt      |
/// | `Deny`      | allow          | deny             | deny        | deny        | deny         | deny        |
///
/// `ControlPlane` always prompts under non-`Deny` modes — kernel admin actions
/// must surface even when the operator has opted into auto-approval.
/// `ReadonlyScoped` is always allowed (the cost of asking exceeds the value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Auto-approve every operation except `ControlPlane`.
    Auto,
    /// Auto-approve readonly operations; prompt for writes/exec/control-plane.
    /// The Claude-Code default — closest to "I'll let you read, ask before edits."
    #[default]
    AskEdit,
    /// Prompt for everything except trivially-cheap `ReadonlyScoped`.
    AskAlways,
    /// Deny anything that would otherwise prompt. Useful for high-assurance
    /// agents that should never escalate at runtime.
    Deny,
}

impl ApprovalMode {
    /// Short, kebab-case name used at the CLI and on the bus.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::AskEdit => "ask_edit",
            Self::AskAlways => "ask_always",
            Self::Deny => "deny",
        }
    }

    /// Parse a CLI-style mode string. Accepts kebab-case and snake_case.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "ask_edit" | "ask-edit" => Ok(Self::AskEdit),
            "ask_always" | "ask-always" => Ok(Self::AskAlways),
            "deny" => Ok(Self::Deny),
            other => Err(format!(
                "unknown approval mode '{}'; expected auto | ask_edit | ask_always | deny",
                other
            )),
        }
    }
}

impl std::fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Outcome of resolving an approval mode against a tool's risk class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run the tool without escalation.
    Allow,
    /// Create a `PendingEscalation` and wait for human resolution.
    Prompt,
    /// Hard-deny the call; audit and fail the tool.
    Deny,
}

impl ApprovalMode {
    /// Apply the mode-vs-risk-class decision matrix. Higher-level overrides
    /// (e.g. a learned "allow always" entry) are applied by the caller; this
    /// function only encodes the coarse default.
    pub fn decide(&self, risk: crate::RiskClass) -> ApprovalDecision {
        use crate::RiskClass::*;
        // ControlPlane is the non-overridable floor for non-Deny modes.
        if matches!(risk, ControlPlane) {
            return match self {
                Self::Deny => ApprovalDecision::Deny,
                _ => ApprovalDecision::Prompt,
            };
        }
        // ReadonlyScoped is always allowed (cost of prompt > value).
        if matches!(risk, ReadonlyScoped) {
            return ApprovalDecision::Allow;
        }
        match self {
            Self::Auto => ApprovalDecision::Allow,
            Self::Deny => ApprovalDecision::Deny,
            Self::AskEdit => match risk {
                ReadonlyScoped | ReadonlyExternal => ApprovalDecision::Allow,
                _ => ApprovalDecision::Prompt,
            },
            Self::AskAlways => match risk {
                ReadonlyScoped => ApprovalDecision::Allow,
                _ => ApprovalDecision::Prompt,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RiskClass::*;

    #[test]
    fn auto_allows_writes_but_prompts_control_plane() {
        assert_eq!(
            ApprovalMode::Auto.decide(WriteScoped),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalMode::Auto.decide(ExecCapable),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalMode::Auto.decide(ControlPlane),
            ApprovalDecision::Prompt
        );
        assert_eq!(
            ApprovalMode::Auto.decide(ReadonlyScoped),
            ApprovalDecision::Allow
        );
    }

    #[test]
    fn ask_edit_allows_reads_prompts_writes() {
        assert_eq!(
            ApprovalMode::AskEdit.decide(ReadonlyScoped),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalMode::AskEdit.decide(ReadonlyExternal),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalMode::AskEdit.decide(WriteScoped),
            ApprovalDecision::Prompt
        );
        assert_eq!(
            ApprovalMode::AskEdit.decide(ExecCapable),
            ApprovalDecision::Prompt
        );
        assert_eq!(
            ApprovalMode::AskEdit.decide(ControlPlane),
            ApprovalDecision::Prompt
        );
    }

    #[test]
    fn ask_always_only_allows_readonly_scoped() {
        assert_eq!(
            ApprovalMode::AskAlways.decide(ReadonlyScoped),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalMode::AskAlways.decide(ReadonlyExternal),
            ApprovalDecision::Prompt
        );
        assert_eq!(
            ApprovalMode::AskAlways.decide(WriteScoped),
            ApprovalDecision::Prompt
        );
    }

    #[test]
    fn deny_blocks_everything_except_readonly_scoped() {
        assert_eq!(
            ApprovalMode::Deny.decide(ReadonlyScoped),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalMode::Deny.decide(ReadonlyExternal),
            ApprovalDecision::Deny
        );
        assert_eq!(
            ApprovalMode::Deny.decide(ControlPlane),
            ApprovalDecision::Deny
        );
    }

    #[test]
    fn parse_round_trips() {
        for m in [
            ApprovalMode::Auto,
            ApprovalMode::AskEdit,
            ApprovalMode::AskAlways,
            ApprovalMode::Deny,
        ] {
            assert_eq!(ApprovalMode::parse(m.as_str()).unwrap(), m);
        }
        assert!(ApprovalMode::parse("ask-edit").is_ok());
        assert!(ApprovalMode::parse("invalid").is_err());
    }
}
