use minijinja::Environment;

const TOOL_DATA_MAX_BYTES: usize = 256 * 1024;

fn escape_html_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn strip_tool_wrapper_tags(s: &str) -> String {
    let trimmed = s.trim();
    let tag_patterns: &[(&str, &str)] = &[
        ("<user_data>", "</user_data>"),
        ("<tool_result>", "</tool_result>"),
        ("<tool_input>", "</tool_input>"),
        ("<result>", "</result>"),
    ];
    for (open, close) in tag_patterns {
        if let Some(rest) = trimmed.strip_prefix(open) {
            if let Some(inner) = rest.strip_suffix(close) {
                return inner.trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

fn deep_parse_tool_json(s: &str, depth: u8) -> serde_json::Value {
    if depth > 4 {
        return serde_json::Value::String(s.to_string());
    }
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(mut v) => {
            deep_resolve_tool_json(&mut v, depth);
            v
        }
        Err(_) => serde_json::Value::String(s.to_string()),
    }
}

fn deep_resolve_tool_json(v: &mut serde_json::Value, depth: u8) {
    match v {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
            {
                *v = deep_parse_tool_json(trimmed, depth + 1);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                deep_resolve_tool_json(item, depth);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                deep_resolve_tool_json(item, depth);
            }
        }
        _ => {}
    }
}

fn unwrap_tool_mcp_envelope(v: &serde_json::Value) -> Option<serde_json::Value> {
    let obj = v.as_object()?;
    let content = obj.get("content")?.as_array()?;
    let texts: Vec<&str> = content
        .iter()
        .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
        .collect();
    if texts.is_empty() {
        return None;
    }
    let combined = texts.join("\n");
    match serde_json::from_str::<serde_json::Value>(&combined) {
        Ok(parsed) => Some(parsed),
        Err(_) => Some(serde_json::Value::String(combined)),
    }
}

fn parsed_tool_value(value: &minijinja::Value) -> serde_json::Value {
    let raw = match value.as_str() {
        Some(s) => s.to_string(),
        None => {
            let json_value = serde_json::to_value(value)
                .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
            serde_json::to_string_pretty(&json_value).unwrap_or_else(|_| json_value.to_string())
        }
    };

    let stripped = strip_tool_wrapper_tags(&raw);
    let mut parsed = deep_parse_tool_json(&stripped, 0);
    if let Some(inner) = unwrap_tool_mcp_envelope(&parsed) {
        parsed = inner;
    }
    deep_resolve_tool_json(&mut parsed, 0);
    parsed
}

fn truncate_tool_data(mut rendered: String) -> String {
    if rendered.len() > TOOL_DATA_MAX_BYTES {
        let mut end = TOOL_DATA_MAX_BYTES;
        while end > 0 && !rendered.is_char_boundary(end) {
            end -= 1;
        }
        rendered.truncate(end);
        rendered.push_str("\n… [truncated]");
    }
    rendered
}

fn render_tool_data_text(value: &minijinja::Value) -> String {
    let parsed = parsed_tool_value(value);
    let rendered = match &parsed {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    truncate_tool_data(rendered)
}

fn render_tool_json_html(value: &serde_json::Value, depth: usize) -> String {
    match value {
        serde_json::Value::Null => "<span class=\"chat-tool-empty\">empty</span>".to_string(),
        serde_json::Value::Bool(v) => {
            format!("<span class=\"chat-tool-scalar\">{v}</span>")
        }
        serde_json::Value::Number(v) => {
            format!("<span class=\"chat-tool-scalar\">{v}</span>")
        }
        serde_json::Value::String(s) => {
            let escaped = escape_html_text(s);
            if s.contains('\n') || s.len() > 120 {
                format!("<pre class=\"chat-tool-text\">{escaped}</pre>")
            } else if s.is_empty() {
                "<span class=\"chat-tool-empty\">empty string</span>".to_string()
            } else {
                format!("<span class=\"chat-tool-string\">{escaped}</span>")
            }
        }
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                return "<span class=\"chat-tool-empty\">empty list</span>".to_string();
            }
            let mut out = String::from("<ol class=\"chat-tool-list\">");
            for item in items {
                out.push_str("<li>");
                out.push_str(&render_tool_json_html(item, depth + 1));
                out.push_str("</li>");
            }
            out.push_str("</ol>");
            out
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return "<span class=\"chat-tool-empty\">empty object</span>".to_string();
            }
            let mut out = String::from("<dl class=\"chat-tool-kv\">");
            for (key, child) in map {
                let key = escape_html_text(key);
                out.push_str("<dt>");
                out.push_str(&key);
                out.push_str("</dt><dd>");
                let is_nested = matches!(
                    child,
                    serde_json::Value::Object(_) | serde_json::Value::Array(_)
                );
                if is_nested && depth < 4 {
                    out.push_str("<details class=\"chat-tool-nested\" open><summary>View ");
                    out.push_str(&key);
                    out.push_str("</summary>");
                    out.push_str(&render_tool_json_html(child, depth + 1));
                    out.push_str("</details>");
                } else {
                    out.push_str(&render_tool_json_html(child, depth + 1));
                }
                out.push_str("</dd>");
            }
            out.push_str("</dl>");
            out
        }
    }
}

fn render_tool_data_html(value: &minijinja::Value) -> minijinja::Value {
    let parsed = parsed_tool_value(value);
    let html = format!(
        "<div class=\"chat-tool-formatted\">{}</div>",
        render_tool_json_html(&parsed, 0)
    );
    minijinja::Value::from_safe_string(html)
}

pub fn build_template_engine() -> Result<Environment<'static>, minijinja::Error> {
    let mut env = Environment::new();

    // Explicitly enable HTML auto-escaping for all .html templates.
    env.set_auto_escape_callback(|template_name| {
        if template_name.ends_with(".html") {
            minijinja::AutoEscape::Html
        } else {
            minijinja::AutoEscape::None
        }
    });

    // `truncate` is shipped only with minijinja-contrib; templates expect it as
    // `value | truncate(length)` so register a small char-safe implementation.
    env.add_filter("truncate", |value: String, length: usize| -> String {
        if value.chars().count() <= length {
            value
        } else {
            let head: String = value.chars().take(length).collect();
            format!("{}…", head)
        }
    });

    env.add_filter("human_role", |value: String| -> String {
        let v = value.trim().to_lowercase();
        match v.as_str() {
            "user" => "User".to_string(),
            "assistant" => "Assistant".to_string(),
            "system" => "System".to_string(),
            "tool" => "Tool".to_string(),
            _ => value,
        }
    });

    env.add_filter("human_intent", |value: String| -> String {
        let v = value.trim().to_lowercase();
        match v.as_str() {
            "read" => "Read".to_string(),
            "write" => "Write".to_string(),
            "execute" => "Execute".to_string(),
            "query" => "Query".to_string(),
            "observe" => "Observe".to_string(),
            "delegate" => "Delegate".to_string(),
            "message" => "Message".to_string(),
            "broadcast" => "Broadcast".to_string(),
            "escalate" => "Escalate".to_string(),
            "subscribe" => "Subscribe".to_string(),
            "unsubscribe" => "Unsubscribe".to_string(),
            _ => value,
        }
    });

    // ── markdown ───────────────────────────────────────────────────────────
    // Server-side markdown → HTML using pulldown-cmark. The result is marked
    // as safe (no auto-escape) because the source is either LLM output that
    // the template already renders inside a sandbox, or known-safe internal
    // text.  For streaming chat the client does its own rendering; this
    // filter is used for stored messages and agent descriptions.
    env.add_filter("markdown", |value: String| -> minijinja::Value {
        use pulldown_cmark::{html, Event, Options, Parser, Tag};

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);

        let parser = Parser::new_ext(&value, options).filter_map(|event| match event {
            Event::Html(raw) | Event::InlineHtml(raw) => Some(Event::Text(raw)),
            Event::Start(Tag::HtmlBlock) | Event::End(pulldown_cmark::TagEnd::HtmlBlock) => None,
            _ => Some(event),
        });
        let mut out = String::new();
        html::push_html(&mut out, parser);
        // Mark the output safe so minijinja does not double-escape the HTML.
        minijinja::Value::from_safe_string(out)
    });

    // ── humanize_event_type ──────────────────────────────────────────────
    // "ToolExecuted" / "TOOL_EXECUTED" → "Tool Executed"
    env.add_filter("humanize_event_type", |value: String| -> String {
        let s = value.replace('_', "");
        let mut out = String::with_capacity(s.len() + 4);
        let mut prev_lower = false;
        for c in s.chars() {
            if c.is_uppercase() {
                if prev_lower {
                    out.push(' ');
                }
                out.push(c);
                prev_lower = false;
            } else {
                out.push(c);
                prev_lower = true;
            }
        }
        out
    });

    // ── relative_time ────────────────────────────────────────────────────
    // "2026-04-11T11:25:49Z" → "2m ago" / "3h ago" / "Apr 9 2026"
    env.add_filter("relative_time", |value: String| -> String {
        let dt = match chrono::DateTime::parse_from_rfc3339(&value) {
            Ok(d) => d.with_timezone(&chrono::Utc),
            Err(_) => return value,
        };
        let now = chrono::Utc::now();
        let diff = now.signed_duration_since(dt);
        if diff < chrono::Duration::seconds(10) {
            "just now".to_string()
        } else if diff < chrono::Duration::minutes(1) {
            format!("{}s ago", diff.num_seconds())
        } else if diff < chrono::Duration::hours(1) {
            format!("{}m ago", diff.num_minutes())
        } else if diff < chrono::Duration::hours(24) {
            format!("{}h ago", diff.num_hours())
        } else if diff < chrono::Duration::days(7) {
            format!("{}d ago", diff.num_days())
        } else {
            dt.format("%b %-d %Y").to_string()
        }
    });

    // ── bytes_human ──────────────────────────────────────────────────────
    env.add_filter("bytes_human", |value: u64| -> String {
        const KB: f64 = 1024.0;
        const MB: f64 = KB * 1024.0;
        const GB: f64 = MB * 1024.0;
        let v = value as f64;
        if v >= GB {
            format!("{:.1} GB", v / GB)
        } else if v >= MB {
            format!("{:.1} MB", v / MB)
        } else if v >= KB {
            format!("{:.1} KB", v / KB)
        } else {
            format!("{} B", value)
        }
    });

    // ── truncate_middle ──────────────────────────────────────────────────
    // "abcdefghijkl", 6 → "ab…kl"
    env.add_filter(
        "truncate_middle",
        |value: String, length: usize| -> String {
            let chars: Vec<char> = value.chars().collect();
            if chars.len() <= length {
                return value;
            }
            let half = length / 2;
            let head: String = chars[..half].iter().collect();
            let tail: String = chars[chars.len() - half..].iter().collect();
            format!("{head}…{tail}")
        },
    );

    // ── pretty_json ──────────────────────────────────────────────────────
    env.add_filter("pretty_json", |value: minijinja::Value| -> String {
        const MAX_BYTES: usize = 256 * 1024;

        fn prettify(json: &serde_json::Value) -> String {
            serde_json::to_string_pretty(json).unwrap_or_else(|_| json.to_string())
        }

        let mut rendered = if let Some(s) = value.as_str() {
            match serde_json::from_str::<serde_json::Value>(s) {
                Ok(v) => prettify(&v),
                // Not valid JSON — return the raw string without wrapping it in quotes.
                Err(_) => s.to_string(),
            }
        } else {
            let json_value = serde_json::to_value(&value)
                .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
            prettify(&json_value)
        };
        if rendered.len() > MAX_BYTES {
            let mut end = MAX_BYTES;
            while end > 0 && !rendered.is_char_boundary(end) {
                end -= 1;
            }
            rendered.truncate(end);
            rendered.push_str("\n… [truncated]");
        }
        rendered
    });

    // ── format_tool_data ──────────────────────────────────────────────────
    // Extracts readable content from raw tool call JSON for legacy <pre> views.
    env.add_filter("format_tool_data", |value: minijinja::Value| -> String {
        render_tool_data_text(&value)
    });

    // Same parsed data, rendered as semantic HTML for chat tool cards.
    env.add_filter("format_tool_data_html", |value: minijinja::Value| {
        render_tool_data_html(&value)
    });

    env.add_template("base.html", include_str!("templates/base.html"))?;
    env.add_template("dashboard.html", include_str!("templates/dashboard.html"))?;
    env.add_template("agents.html", include_str!("templates/agents.html"))?;
    env.add_template("tasks.html", include_str!("templates/tasks.html"))?;
    env.add_template(
        "task_detail.html",
        include_str!("templates/task_detail.html"),
    )?;
    env.add_template("task_trace.html", include_str!("templates/task_trace.html"))?;
    env.add_template("tools.html", include_str!("templates/tools.html"))?;
    env.add_template("secrets.html", include_str!("templates/secrets.html"))?;
    env.add_template("pipelines.html", include_str!("templates/pipelines.html"))?;
    env.add_template(
        "pipelines/list.html",
        include_str!("templates/pipelines/list.html"),
    )?;
    env.add_template(
        "pipelines/builder.html",
        include_str!("templates/pipelines/builder.html"),
    )?;
    env.add_template("audit.html", include_str!("templates/audit.html"))?;
    env.add_template(
        "marketplace.html",
        include_str!("templates/marketplace.html"),
    )?;
    env.add_template(
        "marketplace_detail.html",
        include_str!("templates/marketplace_detail.html"),
    )?;
    env.add_template(
        "audit_detail.html",
        include_str!("templates/audit_detail.html"),
    )?;
    env.add_template(
        "agent_convo_list.html",
        include_str!("templates/agent_convo_list.html"),
    )?;
    env.add_template(
        "agent_convo.html",
        include_str!("templates/agent_convo.html"),
    )?;
    env.add_template("files.html", include_str!("templates/files.html"))?;
    env.add_template("chat.html", include_str!("templates/chat.html"))?;
    env.add_template(
        "chat_conversation.html",
        include_str!("templates/chat_conversation.html"),
    )?;
    env.add_template("management.html", include_str!("templates/management.html"))?;
    env.add_template("plugins.html", include_str!("templates/plugins.html"))?;
    env.add_template(
        "plugin_detail.html",
        include_str!("templates/plugin_detail.html"),
    )?;
    env.add_template("channels.html", include_str!("templates/channels.html"))?;
    env.add_template("schedules.html", include_str!("templates/schedules.html"))?;
    env.add_template("roles.html", include_str!("templates/roles.html"))?;
    env.add_template(
        "role_detail.html",
        include_str!("templates/role_detail.html"),
    )?;
    env.add_template(
        "config_page.html",
        include_str!("templates/config_page.html"),
    )?;
    env.add_template(
        "escalations.html",
        include_str!("templates/escalations.html"),
    )?;
    env.add_template("mcp_page.html", include_str!("templates/mcp_page.html"))?;
    env.add_template(
        "webhooks_page.html",
        include_str!("templates/webhooks_page.html"),
    )?;
    env.add_template("doctor.html", include_str!("templates/doctor.html"))?;
    env.add_template("manual.html", include_str!("templates/manual.html"))?;
    env.add_template("scratchpad.html", include_str!("templates/scratchpad.html"))?;
    env.add_template(
        "resources_page.html",
        include_str!("templates/resources_page.html"),
    )?;
    env.add_template("events_log.html", include_str!("templates/events_log.html"))?;
    env.add_template("logs.html", include_str!("templates/logs.html"))?;
    env.add_template("hal.html", include_str!("templates/hal.html"))?;
    env.add_template("teams.html", include_str!("templates/teams.html"))?;
    env.add_template(
        "teams_detail.html",
        include_str!("templates/teams_detail.html"),
    )?;
    env.add_template("a2a.html", include_str!("templates/a2a.html"))?;
    env.add_template("identity.html", include_str!("templates/identity.html"))?;
    env.add_template(
        "task_snapshots.html",
        include_str!("templates/task_snapshots.html"),
    )?;
    env.add_template(
        "observability.html",
        include_str!("templates/observability.html"),
    )?;
    env.add_template("connectors.html", include_str!("templates/connectors.html"))?;

    // Agent detail page
    env.add_template(
        "agents/detail.html",
        include_str!("templates/agents/detail.html"),
    )?;

    // Cost dashboard
    env.add_template(
        "costs/dashboard.html",
        include_str!("templates/costs/dashboard.html"),
    )?;

    // Notification pages and partials (UNIS Phase 2)
    env.add_template(
        "notifications/inbox.html",
        include_str!("templates/notifications/inbox.html"),
    )?;
    env.add_template(
        "notifications/detail.html",
        include_str!("templates/notifications/detail.html"),
    )?;
    env.add_template(
        "notifications/_notification_row.html",
        include_str!("templates/notifications/_notification_row.html"),
    )?;
    env.add_template(
        "notifications/_notification_list.html",
        include_str!("templates/notifications/_notification_list.html"),
    )?;
    env.add_template(
        "notifications/_respond_form.html",
        include_str!("templates/notifications/_respond_form.html"),
    )?;

    // Partials
    env.add_template(
        "partials/agent_card.html",
        include_str!("templates/partials/agent_card.html"),
    )?;
    env.add_template(
        "partials/task_row.html",
        include_str!("templates/partials/task_row.html"),
    )?;
    env.add_template(
        "partials/tool_card.html",
        include_str!("templates/partials/tool_card.html"),
    )?;
    env.add_template(
        "partials/log_line.html",
        include_str!("templates/partials/log_line.html"),
    )?;
    env.add_template(
        "partials/pipeline_row.html",
        include_str!("templates/partials/pipeline_row.html"),
    )?;
    env.add_template(
        "partials/secret_row.html",
        include_str!("templates/partials/secret_row.html"),
    )?;
    env.add_template(
        "partials/dashboard_stats.html",
        include_str!("templates/partials/dashboard_stats.html"),
    )?;
    env.add_template(
        "partials/dashboard_agents.html",
        include_str!("templates/partials/dashboard_agents.html"),
    )?;
    env.add_template(
        "partials/dashboard_tasks.html",
        include_str!("templates/partials/dashboard_tasks.html"),
    )?;
    env.add_template(
        "partials/dashboard_audit.html",
        include_str!("templates/partials/dashboard_audit.html"),
    )?;
    env.add_template(
        "partials/empty_state.html",
        include_str!("templates/partials/empty_state.html"),
    )?;
    env.add_template(
        "partials/management_page.html",
        include_str!("templates/partials/management_page.html"),
    )?;
    env.add_template(
        "partials/marketplace_reviews.html",
        include_str!("templates/partials/marketplace_reviews.html"),
    )?;
    env.add_template(
        "partials/toast_container.html",
        include_str!("templates/partials/toast_container.html"),
    )?;
    env.add_template(
        "partials/chat_user_msg.html",
        include_str!("templates/partials/chat_user_msg.html"),
    )?;
    env.add_template(
        "partials/chat_assistant_msg.html",
        include_str!("templates/partials/chat_assistant_msg.html"),
    )?;
    env.add_template(
        "partials/chat_tool_call.html",
        include_str!("templates/partials/chat_tool_call.html"),
    )?;
    env.add_template(
        "partials/chat_empty_state.html",
        include_str!("templates/partials/chat_empty_state.html"),
    )?;
    env.add_template(
        "partials/chat_stream_target.html",
        include_str!("templates/partials/chat_stream_target.html"),
    )?;
    env.add_template(
        "partials/shortcuts_modal.html",
        include_str!("templates/partials/shortcuts_modal.html"),
    )?;
    env.add_template(
        "partials/manual_section.html",
        include_str!("templates/partials/manual_section.html"),
    )?;
    env.add_template(
        "partials/manual_raw.html",
        include_str!("templates/partials/manual_raw.html"),
    )?;
    env.add_template(
        "partials/manual_error.html",
        include_str!("templates/partials/manual_error.html"),
    )?;

    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::build_template_engine;
    use minijinja::context;

    #[test]
    fn chat_conversation_template_renders() {
        let env = build_template_engine().expect("template engine should initialize");
        let tpl = env
            .get_template("chat_conversation.html")
            .expect("chat template should exist");
        let rendered = tpl
            .render(context! {
                page_title => "Chat",
                csrf_token => "csrf",
                session_title => "Session",
                session_id => "123e4567-e89b-12d3-a456-426614174000",
                agent_name => "agent",
                agent_initial => "A",
                needs_stream_reconnect => false,
                messages => vec![
                    context! { role => "user", content => "hello", created_at => "2026-04-12T00:00:00Z" },
                    context! { role => "assistant", content => "hi", created_at => "2026-04-12T00:00:01Z" },
                ],
                breadcrumbs => Vec::<minijinja::Value>::new(),
            })
            .expect("chat template should render");
        assert!(rendered.contains("chat-messages-list"));
        assert!(rendered.contains("chat-reply-form"));
    }

    #[test]
    fn manual_section_renders_nested_structure() {
        let env = build_template_engine().expect("template engine should initialize");
        let tpl = env
            .get_template("partials/manual_section.html")
            .expect("manual_section template should exist");
        // Mimic the shape of section_permissions output: object with array-of-objects
        // and nested string fields. Tests the recursive macro path.
        let data = serde_json::json!({
            "section": "permissions",
            "model": "resource:rwx",
            "resource_classes": [
                {"resource": "fs.user_data", "description": "files", "typical_ops": "r, w"},
                {"resource": "memory.semantic", "description": "memory", "typical_ops": "r, w"}
            ],
            "deny_entries": "Deny precedence note",
        });
        let pretty = serde_json::to_string_pretty(&data).unwrap();
        let rendered = tpl
            .render(context! {
                section => "permissions",
                name => Option::<String>::None,
                query => Option::<String>::None,
                data => data,
                pretty => pretty,
                raw => false,
            })
            .expect("manual_section should render");
        assert!(rendered.contains("Resource Classes"));
        assert!(rendered.contains("fs.user_data"));
        assert!(rendered.contains("Deny Entries"));
    }

    #[test]
    fn manual_section_renders_tools_grid() {
        let env = build_template_engine().expect("template engine should initialize");
        let tpl = env
            .get_template("partials/manual_section.html")
            .expect("manual_section template should exist");
        let data = serde_json::json!({
            "section": "tools",
            "count": 2,
            "page": 0,
            "page_size": 20,
            "tools": [
                {
                    "name": "shell-exec",
                    "description": "run a shell",
                    "category": "core",
                    "tags": ["exec"],
                    "permissions": ["process.exec:x"],
                    "trust_tier": "core",
                    "risk_class": "exec_capable"
                }
            ]
        });
        let pretty = serde_json::to_string_pretty(&data).unwrap();
        let rendered = tpl
            .render(context! {
                section => "tools",
                name => Option::<String>::None,
                query => Option::<String>::None,
                data => data,
                pretty => pretty,
                raw => false,
            })
            .expect("manual_section tools should render");
        assert!(rendered.contains("shell-exec"));
        assert!(rendered.contains("/manual/view?section=tool-detail&name=shell-exec"));
    }
}
