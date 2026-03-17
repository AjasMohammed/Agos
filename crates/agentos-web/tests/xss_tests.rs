use agentos_web::templates::build_template_engine;
use minijinja::context;

#[test]
fn test_task_detail_escapes_prompt_xss() {
    let env = build_template_engine().unwrap();
    let tmpl = env.get_template("task_detail.html").unwrap();

    let xss_payload = "<script>alert('xss')</script>";
    let ctx = context! {
        page_title => "Test Task",
        task_id => "12345678-1234-1234-1234-123456789abc",
        state => "Running",
        agent_id => "abcdefab-1234-1234-1234-123456789abc",
        prompt => xss_payload,
        created_at => "2026-03-17 12:00:00",
        priority => 5,
        history => Vec::<minijinja::Value>::new(),
    };

    let rendered = tmpl.render(ctx).unwrap();

    assert!(
        !rendered.contains("<script>alert('xss')</script>"),
        "XSS payload was not escaped in task_detail prompt"
    );
    assert!(
        rendered.contains("&lt;script&gt;"),
        "Escaped script tag not found in output"
    );
}

#[test]
fn test_agent_name_escapes_xss() {
    let env = build_template_engine().unwrap();
    let tmpl = env.get_template("partials/agent_card.html").unwrap();

    let xss_name = "<img src=x onerror=alert(1)>";
    let agents = vec![context! {
        id => "12345678-1234-1234-1234-123456789abc",
        name => xss_name,
        provider => "Ollama",
        model => "llama3",
        status => "Idle",
        description => "test",
        roles => Vec::<String>::new(),
        current_task => Option::<String>::None,
        created_at => "2026-03-17",
        last_active => "2026-03-17",
    }];

    let ctx = context! { agents };
    let rendered = tmpl.render(ctx).unwrap();

    assert!(
        !rendered.contains("<img src=x onerror=alert(1)>"),
        "XSS payload was not escaped in agent name"
    );
    assert!(
        rendered.contains("&lt;img"),
        "Escaped img tag not found in output"
    );
}

#[test]
fn test_audit_details_escapes_xss() {
    let env = build_template_engine().unwrap();
    let tmpl = env.get_template("partials/log_line.html").unwrap();

    let xss_details = "<script>document.cookie</script>";
    let entries = vec![context! {
        timestamp => "2026-03-17 12:00:00",
        event_type => "TaskStarted",
        severity => "Info",
        agent_id => Option::<String>::None,
        task_id => Option::<String>::None,
        tool_id => Option::<String>::None,
        details => xss_details,
    }];

    let ctx = context! {
        page_title => "Audit Log",
        entries,
        total_count => 1u64,
    };

    let rendered = tmpl.render(ctx).unwrap();
    assert!(
        !rendered.contains("<script>document.cookie</script>"),
        "XSS payload was not escaped in audit details"
    );
    assert!(
        rendered.contains("&lt;script&gt;"),
        "Escaped script tag not found in output"
    );
}

#[test]
fn test_pipeline_name_not_injected_into_js_context() {
    let env = build_template_engine().unwrap();
    let tmpl = env.get_template("partials/pipeline_row.html").unwrap();

    // A name containing characters that would break a JavaScript string literal.
    // Backslash and single-quote are not HTML-special, so HTML auto-escaping
    // would NOT protect against them if they were embedded in a JS string.
    // The fix uses a data-attribute instead, so the name lands only in an HTML
    // attribute context where auto-escaping is sufficient.
    let xss_name = "\\'; alert(document.cookie); //";
    let pipelines = vec![context! {
        name => xss_name,
        version => "1.0",
        description => "test pipeline",
        step_count => 3,
        installed_at => "2026-03-17",
    }];

    let ctx = context! { pipelines };
    let rendered = tmpl.render(ctx).unwrap();

    // The raw JS-breaking payload must not appear inside a JS string context.
    // With the data-attribute fix the name appears only as an HTML attribute value.
    assert!(
        !rendered.contains("selectedPipeline = '"),
        "Pipeline name must not be embedded inside a JS string literal"
    );
    // The name should appear as a data attribute (HTML-escaped).
    assert!(
        rendered.contains("data-pipeline="),
        "Pipeline name must be placed in a data-pipeline attribute"
    );
}

#[test]
fn test_zeroizing_string_takes_ownership() {
    use agentos_vault::ZeroizingString;
    let mut original = String::from("super-secret-value");
    let secret = ZeroizingString::new(std::mem::take(&mut original));
    assert!(
        original.is_empty(),
        "Original string should be emptied after take"
    );
    assert_eq!(secret.as_str(), "super-secret-value");
}
