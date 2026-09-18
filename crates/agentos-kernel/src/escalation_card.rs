//! Channel-ready rendering of a [`PendingEscalation`].
//!
//! `ApprovalHook` writes the operator-facing text as a labelled block
//! (`Who:` / `What:` / `Where:` / `Why you are asked:` / `Details:`), which
//! the panel shows as-is. A chat message needs more shape than that: the
//! payload JSON as readable fields, the risk and expiry at a glance, and
//! agent-controlled text that cannot break the channel's markup. This module
//! parses the escalation once into an [`EscalationCard`] and renders it as
//! markdown — converted to HTML by the Telegram adapter and sent as-is to the
//! markdown-native channels (Discord, Matrix, Mattermost, Teams).
//!
//! Two variants, because the audiences differ:
//! - [`EscalationCard::to_markdown`] — the full card, only for channels paired
//!   to the operator (a Telegram private chat, a paired DM).
//! - [`EscalationCard::to_summary_markdown`] — what third-party delivery
//!   targets (ntfy, webhook, desktop, the inbox) get: the question, agent,
//!   tool and risk, never the task text or the payload.

use crate::escalation::{AutoAction, PendingEscalation};

/// Max payload fields listed before the rest are counted.
const MAX_DETAIL_FIELDS: usize = 12;
/// Per-value clip. Long enough for a real path, short enough for a phone.
const MAX_VALUE_CHARS: usize = 200;
/// Budget for free-form summary text.
const MAX_NOTES_CHARS: usize = 700;

pub struct EscalationCard {
    id: u64,
    question: String,
    agent: Option<String>,
    task: Option<String>,
    tool: Option<String>,
    tool_about: Option<String>,
    target: Option<String>,
    /// Raw class name, e.g. `ControlPlane`.
    risk_class: Option<String>,
    risk_explanation: Option<String>,
    mode: Option<String>,
    details: Details,
    /// Lines that are not part of the labelled block — the whole summary for
    /// escalations raised by something other than `ApprovalHook`.
    notes: Vec<String>,
    urgency: String,
    expires_at: chrono::DateTime<chrono::Utc>,
    auto_action: AutoAction,
}

enum Details {
    None,
    Fields(Vec<(String, String)>),
    /// Unparseable (or clipped) payload text.
    Raw(String),
}

impl EscalationCard {
    pub fn from_escalation(esc: &PendingEscalation) -> Self {
        let mut card = Self {
            id: esc.id,
            question: esc.decision_point.trim().to_string(),
            agent: None,
            task: None,
            tool: None,
            tool_about: None,
            target: None,
            risk_class: None,
            risk_explanation: None,
            mode: None,
            details: Details::None,
            notes: Vec::new(),
            urgency: esc.urgency.clone(),
            expires_at: esc.expires_at,
            auto_action: esc.auto_action,
        };
        let mut details_raw = None;

        for line in esc.context_summary.lines().map(str::trim) {
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("Who: ") {
                match rest.split_once(", while working on ") {
                    Some((agent, task)) => {
                        card.agent = Some(agent.to_string());
                        card.task = Some(task.trim_matches('"').to_string());
                    }
                    None => card.agent = Some(rest.to_string()),
                }
            } else if let Some(rest) = line.strip_prefix("What: ") {
                match rest.split_once(" — ") {
                    Some((tool, about)) => {
                        card.tool = Some(tool.to_string());
                        card.tool_about = Some(about.to_string());
                    }
                    None => card.tool = Some(rest.to_string()),
                }
            } else if let Some(rest) = line.strip_prefix("Where: ") {
                card.target = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("Why you are asked: ") {
                let (risk, mode) = match rest.split_once("; approval mode is ") {
                    Some((r, m)) => (r, Some(m.trim_matches('`').to_string())),
                    None => (rest, None),
                };
                card.mode = mode;
                match risk.split_once(" — ") {
                    Some((class, why)) => {
                        card.risk_class = Some(class.to_string());
                        card.risk_explanation = Some(why.to_string());
                    }
                    None => card.risk_class = Some(risk.to_string()),
                }
            } else if let Some(rest) = line.strip_prefix("Details: ") {
                details_raw = Some(rest.to_string());
            } else {
                card.notes.push(line.to_string());
            }
        }

        // The summary's `Details:` is clipped to 300 chars, which cuts most real
        // payloads mid-JSON. `ApprovalHook` also stores the (redacted) payload
        // as `metadata.input`; prefer that and fall back to the text.
        card.details = match esc.metadata.get("input") {
            Some(serde_json::Value::Object(map)) => Details::Fields(fields_of(map)),
            _ => match details_raw {
                Some(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(serde_json::Value::Object(map)) => Details::Fields(fields_of(&map)),
                    _ => Details::Raw(raw),
                },
                None => Details::None,
            },
        };
        if card.tool.is_none() {
            card.tool = esc
                .metadata
                .get("tool_name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        card
    }

    /// Full card for operator-paired channels.
    pub fn to_markdown(&self) -> String {
        let mut out = self.header();

        let mut facts = Vec::new();
        if let Some(agent) = &self.agent {
            facts.push(format!("👤 **Agent:** {}", md_text(agent)));
        }
        if let Some(task) = &self.task {
            facts.push(format!("📋 **Task:** {}", md_text(task)));
        }
        if let Some(tool) = &self.tool {
            facts.push(format!("🔧 **Tool:** {}", md_code(tool)));
            if let Some(about) = &self.tool_about {
                facts.push(format!("      {}", md_text(about)));
            }
        }
        if let Some(target) = &self.target {
            facts.push(format!("📍 **Target:** {}", md_code(target)));
        }
        if let Some(risk) = self.risk_line() {
            facts.push(risk);
        }
        if let Some(mode) = &self.mode {
            facts.push(format!("⚙️ **Approval mode:** {}", md_code(mode)));
        }
        push_section(&mut out, &facts);

        match &self.details {
            Details::None => {}
            Details::Fields(fields) if fields.is_empty() => {}
            Details::Fields(fields) => {
                let mut lines = vec!["**Request details**".to_string()];
                for (key, value) in fields.iter().take(MAX_DETAIL_FIELDS) {
                    lines.push(format!("- {}: {}", md_text(key), md_code(value)));
                }
                if fields.len() > MAX_DETAIL_FIELDS {
                    lines.push(format!(
                        "- … {} more in the AgentOS panel",
                        fields.len() - MAX_DETAIL_FIELDS
                    ));
                }
                push_section(&mut out, &lines);
            }
            Details::Raw(raw) => push_section(
                &mut out,
                &["**Request details**".to_string(), md_code(&clip(raw, 400))],
            ),
        }

        // Free-form escalations put their whole text here; bound it like the
        // old 700-char context preview so one long summary cannot fill a phone.
        let notes: Vec<String> = clip(&self.notes.join("\n"), MAX_NOTES_CHARS)
            .lines()
            .map(md_text)
            .collect();
        push_section(&mut out, &notes);

        push_section(&mut out, &[self.footer()]);
        out
    }

    /// Redacted card for third-party delivery targets: no task text, target
    /// line, payload or free-form notes. The question already names the
    /// target, which is what it has always carried on this path.
    pub fn to_summary_markdown(&self) -> String {
        let mut out = self.header();
        let mut facts = Vec::new();
        if let Some(agent) = &self.agent {
            facts.push(format!("👤 **Agent:** {}", md_text(agent)));
        }
        if let Some(tool) = &self.tool {
            facts.push(format!("🔧 **Tool:** {}", md_code(tool)));
        }
        if let Some(risk) = self.risk_line() {
            facts.push(risk);
        }
        push_section(&mut out, &facts);
        push_section(
            &mut out,
            &[
                "Full request details are in the AgentOS panel.".to_string(),
                self.footer(),
            ],
        );
        out
    }

    /// Plain one-line title, used as `UserMessage.subject` (ntfy's Title
    /// header, desktop notifications) — so it must carry no markup. It is also
    /// the first line of both renders, which is what lets an adapter that
    /// prints `subject` above `body` detect the duplicate.
    pub fn title(&self) -> String {
        format!("🛂 Approval needed · #{}", self.id)
    }

    fn header(&self) -> String {
        format!("{}\n**{}**", self.title(), md_text(&self.question))
    }

    fn risk_line(&self) -> Option<String> {
        let class = self.risk_class.as_deref()?;
        let (icon, label) = risk_label(class);
        Some(match &self.risk_explanation {
            Some(why) => format!("{icon} **Risk:** {label} — {}", md_text(why)),
            None => format!("{icon} **Risk:** {label}"),
        })
    }

    fn footer(&self) -> String {
        let urgency = self.urgency.trim().to_lowercase();
        let icon = match urgency.as_str() {
            "critical" => "🚨",
            "high" => "🔴",
            "low" => "🟢",
            _ => "🟠",
        };
        let left = (self.expires_at - chrono::Utc::now()).num_seconds().max(0);
        let verb = match self.auto_action {
            AutoAction::Deny => "Auto-denies",
            AutoAction::Approve => "Auto-approves",
        };
        let when = if left >= 60 {
            format!("{}m {:02}s", left / 60, left % 60)
        } else {
            format!("{left}s")
        };
        format!(
            "{icon} {} urgency · ⏳ {verb} in {when}",
            md_text(&capitalize(&urgency))
        )
    }
}

fn push_section(out: &mut String, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    out.push_str("\n\n");
    out.push_str(&lines.join("\n"));
}

fn fields_of(map: &serde_json::Map<String, serde_json::Value>) -> Vec<(String, String)> {
    map.iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            let value = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (clip(k, 40), clip(&value, MAX_VALUE_CHARS))
        })
        .collect()
}

/// `(icon, readable name)` for a `RiskClass` debug name.
fn risk_label(class: &str) -> (&'static str, String) {
    let icon = match class {
        "ReadonlyScoped" | "ReadonlyExternal" => "🟢",
        "WriteAgentState" | "Interactive" => "🟡",
        "WriteScoped" => "🟠",
        "ExecCapable" | "ControlPlane" => "🔴",
        _ => "⚪",
    };
    // `ControlPlane` → `Control plane`.
    let mut label = String::new();
    for (i, c) in class.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            label.push(' ');
            label.extend(c.to_lowercase());
        } else {
            label.push(c);
        }
    }
    (icon, md_text(&label))
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// Agent-controlled prose, safe inside markdown: inline markers are
/// backslash-escaped so a file name or a task sentence cannot open italics or
/// a link, and a newline cannot start a list or header.
///
/// A backtick cannot be escaped — converters pair code spans before they see
/// escapes, so `\`` would still open a span that swallows the rest of the
/// card — so it becomes a quote, as in [`md_code`]. A leading `#`, `>` or `-`
/// is harmless here: every caller puts a label or bullet in front.
fn md_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '*' | '_' | '~' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            '`' => out.push('\''),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// A value shown verbatim as inline code. A backtick would close the span
/// early, and there is no escape inside one, so it is swapped for a quote.
fn md_code(s: &str) -> String {
    let body: String = s
        .chars()
        .map(|c| match c {
            '`' => '\'',
            '\n' | '\r' => ' ',
            c => c,
        })
        .collect();
    if body.trim().is_empty() {
        return "—".to_string();
    }
    format!("`{body}`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel_action::EscalationReason;
    use agentos_types::{AgentID, TaskID, TraceID};

    fn tool_escalation() -> PendingEscalation {
        PendingEscalation {
            id: 604,
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            reason: EscalationReason::AuthorizationRequired,
            context_summary: "Who: OSS, while working on \"play the *new* song\"\n\
                What: audio — List audio devices and play back audio files.\n\
                Where: /home/ajas/Desktop/Ennavale_ennai_song.mp3\n\
                Why you are asked: ControlPlane — changes AgentOS itself (agents, keys, policy) — never auto-approved; approval mode is `ask_edit`\n\
                Details: {\"action\":\"playback\",\"audio_path\":\"/home/ajas/Desk…"
                .into(),
            decision_point: "Allow OSS to use 'audio' on /home/ajas/Desktop/Ennavale_ennai_song.mp3?".into(),
            options: vec!["approve".into(), "deny".into()],
            urgency: "high".into(),
            blocking: true,
            trace_id: TraceID::new(),
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(299),
            auto_action: AutoAction::Deny,
            metadata: serde_json::json!({
                "kind": "tool_approval",
                "tool_name": "audio",
                "input": {"action": "playback", "audio_path": "/home/ajas/Desktop/Ennavale_ennai_song.mp3", "volume": 1, "device": null},
            }),
            resolved: false,
            resolution: None,
            resolved_at: None,
        }
    }

    #[test]
    fn full_card_lays_out_every_part_of_the_request() {
        let md = EscalationCard::from_escalation(&tool_escalation()).to_markdown();
        assert!(md.starts_with(
            "🛂 Approval needed · #604\n**Allow OSS to use 'audio' on /home/ajas/Desktop/Ennavale\\_ennai\\_song.mp3?**"
        ));
        assert!(md.contains("👤 **Agent:** OSS"));
        // Agent-authored task text is escaped, not interpreted.
        assert!(md.contains(r"📋 **Task:** play the \*new\* song"));
        assert!(md.contains("🔧 **Tool:** `audio`"));
        assert!(md.contains("📍 **Target:** `/home/ajas/Desktop/Ennavale_ennai_song.mp3`"));
        assert!(md.contains("🔴 **Risk:** Control plane — changes AgentOS itself"));
        assert!(md.contains("⚙️ **Approval mode:** `ask_edit`"));
        // Structured metadata wins over the clipped `Details:` text; nulls drop.
        assert!(md.contains(r"- audio\_path: `/home/ajas/Desktop/Ennavale_ennai_song.mp3`"));
        assert!(md.contains("- volume: `1`"));
        assert!(!md.contains("- device"));
        assert!(md.contains("🔴 High urgency · ⏳ Auto-denies in 4m"));
    }

    #[test]
    fn summary_withholds_task_target_and_payload() {
        let md = EscalationCard::from_escalation(&tool_escalation()).to_summary_markdown();
        assert!(md.contains("#604") && md.contains("Allow OSS to use 'audio' on"));
        assert!(md.contains("🔧 **Tool:** `audio`"));
        // The question names the target (it always has on this path); the
        // task, the target line and the payload stay out.
        for secret in ["new", "playback", "volume", "Target", "Task"] {
            assert!(!md.contains(secret), "summary leaked {secret}: {md}");
        }
    }

    #[test]
    fn clipped_details_without_metadata_fall_back_to_raw_text() {
        let mut esc = tool_escalation();
        esc.metadata = serde_json::json!({});
        let md = EscalationCard::from_escalation(&esc).to_markdown();
        assert!(md.contains("**Request details**\n`{\"action\":\"playback\""));
    }

    #[test]
    fn free_form_escalation_keeps_its_text_as_notes() {
        let mut esc = tool_escalation();
        esc.context_summary = "Agent wants to install python3\nsecond line".into();
        esc.metadata = serde_json::Value::Null;
        let md = EscalationCard::from_escalation(&esc).to_markdown();
        assert!(md.contains("Agent wants to install python3\nsecond line"));
        assert!(!md.contains("**Agent:**"));
    }

    #[test]
    fn a_lone_backtick_in_agent_text_does_not_shift_the_code_spans() {
        let mut esc = tool_escalation();
        esc.context_summary = esc
            .context_summary
            .replace("play the *new* song", "don't print the ` char");
        let md = EscalationCard::from_escalation(&esc).to_markdown();
        let html = agentos_channels::telegram_format::markdown_to_telegram_html(&md);
        assert!(
            html.contains("🔧 <b>Tool:</b> <code>audio</code>"),
            "{html}"
        );
        assert!(!html.contains("<i>"), "{html}");
    }

    #[test]
    fn markup_in_values_cannot_escape_its_span() {
        assert_eq!(md_code("a`b\nc"), "`a'b c`");
        assert_eq!(md_text("[x](y) _i_\n# h"), r"\[x\](y) \_i\_ # h");
        assert_eq!(md_text("a`b"), "a'b");
        assert_eq!(md_code("  "), "—");
    }

    #[test]
    fn renders_through_telegram_html_without_stray_markup() {
        let md = EscalationCard::from_escalation(&tool_escalation()).to_markdown();
        let html = agentos_channels::telegram_format::markdown_to_telegram_html(&md);
        assert!(html.contains(
            "<b>Allow OSS to use 'audio' on /home/ajas/Desktop/Ennavale_ennai_song.mp3?</b>"
        ));
        assert!(html.contains("play the *new* song"));
        assert!(
            html.contains("• audio_path: <code>/home/ajas/Desktop/Ennavale_ennai_song.mp3</code>")
        );
        assert!(!html.contains("<i>"), "no accidental italics: {html}");
    }
}
