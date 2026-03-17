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
    env.add_template("tools.html", include_str!("templates/tools.html"))?;
    env.add_template("secrets.html", include_str!("templates/secrets.html"))?;
    env.add_template("pipelines.html", include_str!("templates/pipelines.html"))?;
    env.add_template("audit.html", include_str!("templates/audit.html"))?;

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

    Ok(env)
}
