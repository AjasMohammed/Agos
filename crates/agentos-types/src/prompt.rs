//! Actionable controls carried alongside a [`crate::UserMessage`].
//!
//! An approval prompt is not prose. It is a body plus a set of choices, and
//! every delivery channel renders those choices differently — a Telegram inline
//! keyboard, an ntfy `Actions` header, a Discord component row, or, where the
//! platform has no interactive primitive, a line of text. Modelling the choices
//! as data instead of baking `/approve 42` into the body is what lets one
//! escalation reach all of them without each adapter parsing English.

use serde::{Deserialize, Serialize};

/// One actionable control attached to a message.
///
/// `command` is the literal text the inbound router will see when the user
/// activates the control — a button's callback payload, or what the operator
/// types on a channel with no buttons. Keeping those identical is the whole
/// trick: a Telegram `callback_query` already arrives as `InboundMessage.text`,
/// so a tap routes into the existing command handler with no new plumbing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAction {
    /// Human-facing control text, e.g. "✅ Approve".
    pub label: String,
    /// Literal inbound command, e.g. "/approve 42".
    pub command: String,
    #[serde(default)]
    pub style: ActionStyle,
}

/// Visual weight of a control. Adapters map this onto their own vocabulary
/// (Discord button styles, Slack block `style`); adapters without one ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStyle {
    Primary,
    Danger,
    #[default]
    Secondary,
}

impl PromptAction {
    /// Telegram's `callback_data` limit, and the tightest of every platform
    /// that carries a command in a button payload. Exceeding it makes Telegram
    /// reject the whole keyboard, so a longer command must never be emitted as
    /// a button — and must never be *truncated* into one either, since a
    /// truncated `/approve 4` addresses a different escalation.
    pub const MAX_COMMAND_BYTES: usize = 64;

    pub fn new(label: impl Into<String>, command: impl Into<String>, style: ActionStyle) -> Self {
        Self {
            label: label.into(),
            command: command.into(),
            style,
        }
    }

    /// Whether this command fits in a button payload.
    pub fn fits_callback_data(&self) -> bool {
        self.command.len() <= Self::MAX_COMMAND_BYTES
    }

    /// Label clipped to `max` characters, for platforms with a tight cap
    /// (WhatsApp reply buttons allow 20). Clips on a char boundary; the full
    /// label stays available for everyone else.
    pub fn short_label(&self, max: usize) -> String {
        self.label.chars().take(max).collect()
    }
}

/// Shared plain-text rendering for adapters with no interactive primitive.
///
/// Every such adapter calls this, so the operator reads the same wording on
/// Matrix as in email. Returns an empty string for an empty slice so callers
/// can append unconditionally.
pub fn render_actions_fallback(actions: &[PromptAction]) -> String {
    if actions.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n");
    for a in actions {
        out.push_str(&format!("\n{} — reply `{}`", a.label, a.command));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_is_empty_for_no_actions() {
        // Callers append unconditionally; an empty set must add nothing.
        assert_eq!(render_actions_fallback(&[]), "");
    }

    #[test]
    fn fallback_lists_every_command() {
        let actions = vec![
            PromptAction::new("✅ Approve", "/approve 42", ActionStyle::Primary),
            PromptAction::new("❌ Deny", "/deny 42", ActionStyle::Danger),
        ];
        let s = render_actions_fallback(&actions);
        assert!(s.contains("/approve 42"));
        assert!(s.contains("/deny 42"));
        assert!(s.starts_with('\n'), "must separate from the body");
    }

    #[test]
    fn callback_data_limit_is_by_bytes_not_chars() {
        // An emoji label is fine; the guard is on `command`, which is ASCII.
        let ok = PromptAction::new("✅ Approve", "/approve 42", ActionStyle::Primary);
        assert!(ok.fits_callback_data());

        let too_long = PromptAction::new(
            "x",
            "/approve ".to_string() + &"9".repeat(60),
            ActionStyle::Primary,
        );
        assert!(!too_long.fits_callback_data());
    }

    #[test]
    fn short_label_clips_on_char_boundary() {
        let a = PromptAction::new(
            "✅ Approve & always allow",
            "/approve 42 always",
            ActionStyle::Secondary,
        );
        // Would panic on a byte slice — the leading emoji is 3 bytes.
        assert_eq!(a.short_label(3).chars().count(), 3);
        assert_eq!(a.short_label(500), a.label);
    }

    #[test]
    fn style_defaults_to_secondary_when_absent() {
        let a: PromptAction =
            serde_json::from_str(r#"{"label":"Go","command":"/go 1"}"#).expect("deserialize");
        assert_eq!(a.style, ActionStyle::Secondary);
    }
}
