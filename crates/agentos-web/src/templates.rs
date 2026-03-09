use minijinja::Environment;

pub fn build_template_engine() -> Environment<'static> {
    let mut env = Environment::new();

    env.add_template("base.html", include_str!("templates/base.html"))
        .expect("failed to load base.html");
    env.add_template("dashboard.html", include_str!("templates/dashboard.html"))
        .expect("failed to load dashboard.html");
    env.add_template("agents.html", include_str!("templates/agents.html"))
        .expect("failed to load agents.html");
    env.add_template("tasks.html", include_str!("templates/tasks.html"))
        .expect("failed to load tasks.html");
    env.add_template("task_detail.html", include_str!("templates/task_detail.html"))
        .expect("failed to load task_detail.html");
    env.add_template("tools.html", include_str!("templates/tools.html"))
        .expect("failed to load tools.html");
    env.add_template("secrets.html", include_str!("templates/secrets.html"))
        .expect("failed to load secrets.html");
    env.add_template("pipelines.html", include_str!("templates/pipelines.html"))
        .expect("failed to load pipelines.html");
    env.add_template("audit.html", include_str!("templates/audit.html"))
        .expect("failed to load audit.html");

    // Partials
    env.add_template(
        "partials/agent_card.html",
        include_str!("templates/partials/agent_card.html"),
    )
    .expect("failed to load agent_card.html");
    env.add_template(
        "partials/task_row.html",
        include_str!("templates/partials/task_row.html"),
    )
    .expect("failed to load task_row.html");
    env.add_template(
        "partials/tool_card.html",
        include_str!("templates/partials/tool_card.html"),
    )
    .expect("failed to load tool_card.html");
    env.add_template(
        "partials/log_line.html",
        include_str!("templates/partials/log_line.html"),
    )
    .expect("failed to load log_line.html");

    env
}
