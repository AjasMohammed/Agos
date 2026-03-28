use minijinja::Environment;

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
        "audit_detail.html",
        include_str!("templates/audit_detail.html"),
    )?;
    env.add_template("chat.html", include_str!("templates/chat.html"))?;
    env.add_template(
        "chat_conversation.html",
        include_str!("templates/chat_conversation.html"),
    )?;

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
        "partials/toast_container.html",
        include_str!("templates/partials/toast_container.html"),
    )?;
    env.add_template(
        "partials/shortcuts_modal.html",
        include_str!("templates/partials/shortcuts_modal.html"),
    )?;

    Ok(env)
}
