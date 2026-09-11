//! The single escalation → actionable-control converter.
//!
//! Every outbound path (paired DM, notification-router fan-out, webhook
//! payload) sources its controls here, so the button on Telegram, the ntfy
//! action and the plain-text instruction on Matrix always carry the same
//! commands. Adapters never re-derive a command by parsing the rendered body.

use crate::escalation::PendingEscalation;
use agentos_types::{ActionStyle, PromptAction};

/// Build the controls for one escalation.
///
/// Derived from `esc.options` rather than hardcoded, so an escalation that
/// offers a third choice is carried without touching any adapter. The
/// standing-grant control is appended because `InboundRouter` already accepts
/// `/approve <id> always` (see `handle_approval_command`), not because the
/// escalation lists it as an option.
pub fn escalation_actions(esc: &PendingEscalation) -> Vec<PromptAction> {
    let id = esc.id;
    let mut out = Vec::with_capacity(esc.options.len() + 1);
    let mut has_approve = false;

    for opt in &esc.options {
        match opt.as_str() {
            "approve" => {
                has_approve = true;
                out.push(PromptAction::new(
                    "✅ Approve",
                    format!("/approve {id}"),
                    ActionStyle::Primary,
                ));
            }
            "deny" => out.push(PromptAction::new(
                "❌ Deny",
                format!("/deny {id}"),
                ActionStyle::Danger,
            )),
            // `/escalation <id> <option>` is not a command `InboundRouter`
            // accepts today, so such a control is rendered but inert. No
            // escalation currently offers a third option; if one is added, wire
            // the command in `inbound_router.rs` at the same time.
            other => out.push(PromptAction::new(
                sanitize_label(other),
                format!("/escalation {id} {other}"),
                ActionStyle::Secondary,
            )),
        }
    }

    if has_approve {
        out.push(PromptAction::new(
            "✅ Approve & always allow",
            format!("/approve {id} always"),
            ActionStyle::Secondary,
        ));
    }

    // Drop rather than truncate: a clipped `/approve 4` resolves a different
    // escalation than the one the operator was shown.
    //
    // The empty-label check is not cosmetic. `PendingEscalation.options` is
    // agent-supplied (parsed straight off the `escalate_to_human` intent with
    // no validation), and every platform rejects a control with empty button
    // text: Telegram 400s the whole `sendMessage`, Discord rejects the payload,
    // WhatsApp rejects an empty `title`. Because those adapters took the
    // native-control path they have no text fallback to fall back to, so one
    // blank option would suppress the agent's own approval prompt on every
    // button-capable channel and park the task until auto-deny.
    out.retain(|a| a.fits_callback_data() && !a.label.trim().is_empty());
    out
}

/// Clip an agent-supplied option to a safe button label.
///
/// Control characters are dropped, not escaped: they ride inside HTTP headers
/// (ntfy `Actions`) and JSON button payloads, where a stray `\r`/`\n` fails the
/// send outright rather than just spoiling one label.
fn sanitize_label(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel_action::EscalationReason;
    use agentos_types::{AgentID, TaskID, TraceID};

    fn fixture(id: u64, options: Vec<String>) -> PendingEscalation {
        PendingEscalation {
            id,
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            reason: EscalationReason::AuthorizationRequired,
            context_summary: "Agent wants to install python3".into(),
            decision_point: "approve install of python3 via apt-get".into(),
            options,
            urgency: "high".into(),
            blocking: true,
            trace_id: TraceID::new(),
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(300),
            auto_action: crate::escalation::AutoAction::Deny,
            metadata: serde_json::Value::Null,
            resolved: false,
            resolution: None,
            resolved_at: None,
        }
    }

    fn approve_deny(id: u64) -> PendingEscalation {
        fixture(id, vec!["approve".into(), "deny".into()])
    }

    #[test]
    fn covers_approve_deny_and_always() {
        let actions = escalation_actions(&approve_deny(42));
        let cmds: Vec<&str> = actions.iter().map(|a| a.command.as_str()).collect();
        assert_eq!(cmds, ["/approve 42", "/deny 42", "/approve 42 always"]);
        assert_eq!(actions[0].style, ActionStyle::Primary);
        assert_eq!(actions[1].style, ActionStyle::Danger);
    }

    #[test]
    fn every_command_fits_a_button_payload() {
        // u64::MAX is the widest id the store can hand us.
        let actions = escalation_actions(&approve_deny(u64::MAX));
        assert!(!actions.is_empty());
        for a in &actions {
            assert!(
                a.fits_callback_data(),
                "command {:?} exceeds the callback-data cap",
                a.command
            );
        }
    }

    #[test]
    fn oversized_option_is_dropped_not_truncated() {
        // A truncated command would address a different escalation.
        let esc = fixture(1, vec!["approve".into(), "x".repeat(200)]);
        let actions = escalation_actions(&esc);
        let cmds: Vec<&str> = actions.iter().map(|a| a.command.as_str()).collect();
        assert_eq!(cmds, ["/approve 1", "/approve 1 always"]);
    }

    #[test]
    fn no_approve_option_means_no_standing_grant_control() {
        let esc = fixture(7, vec!["deny".into()]);
        let actions = escalation_actions(&esc);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].command, "/deny 7");
    }

    #[test]
    fn a_blank_option_is_dropped_rather_than_killing_the_whole_prompt() {
        // Options come straight off the agent's intent. An empty button label
        // is rejected by Telegram/Discord/WhatsApp and would fail the entire
        // send — on adapters that render controls there is no text fallback,
        // so the operator would see nothing at all.
        let esc = fixture(9, vec!["approve".into(), "  ".into(), "deny".into()]);
        let cmds: Vec<String> = escalation_actions(&esc)
            .into_iter()
            .map(|a| a.command)
            .collect();
        assert_eq!(cmds, ["/approve 9", "/deny 9", "/approve 9 always"]);
    }

    #[test]
    fn control_characters_are_stripped_from_an_option_label() {
        // A raw newline inside an ntfy `Actions` header value makes reqwest
        // refuse to build the header, dropping the escalation entirely.
        let esc = fixture(11, vec!["approve".into(), "we\r\nird".into()]);
        let labels: Vec<String> = escalation_actions(&esc)
            .into_iter()
            .map(|a| a.label)
            .collect();
        assert!(labels
            .iter()
            .all(|l| !l.contains('\n') && !l.contains('\r')));
        assert!(labels.contains(&"weird".to_string()), "got: {labels:?}");
    }

    #[test]
    fn no_options_yields_no_controls() {
        assert!(escalation_actions(&fixture(3, vec![])).is_empty());
    }
}
