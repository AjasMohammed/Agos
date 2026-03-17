---
title: "TODO: Deduplicate Web Templates Using include"
tags:
  - webui
  - next-steps
date: 2026-03-17
status: complete
effort: 1h
priority: low
---

# Deduplicate Web Templates Using include

> Replace inline HTML markup in full-page templates with `{% include "partials/foo.html" %}` to eliminate duplication between full-page and HTMX partial templates.

## Why This Phase

Six full-page templates in `crates/agentos-web/src/templates/` duplicate markup that also exists in the corresponding partial templates under `templates/partials/`. This is a maintainability issue: any UI change must be made in two places. MiniJinja supports `{% include "..." %}`, which makes the full-page templates reuse the partial markup.

This is the only item from WebUI Security Fixes Phase 01 that was not already implemented.

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `agents.html` | Inline agent card markup | `{% include "partials/agent_card.html" %}` |
| `tasks.html` | Inline task row markup | `{% include "partials/task_row.html" %}` |
| `tools.html` | Inline tool card markup | `{% include "partials/tool_card.html" %}` |
| `secrets.html` | Inline secret row markup | `{% include "partials/secret_row.html" %}` |
| `pipelines.html` | Inline pipeline row markup | `{% include "partials/pipeline_row.html" %}` |
| `audit.html` | Inline log line markup | `{% include "partials/log_line.html" %}` |

## Detailed Subtasks

1. Open `crates/agentos-web/src/templates/agents.html`
2. Locate the `{% for agent in agents %}` loop body
3. Read the corresponding partial `crates/agentos-web/src/templates/partials/agent_card.html`
4. Verify the partial references the same variable name as the loop variable (e.g., `agent`)
5. Replace the inline loop body with `{% include "partials/agent_card.html" %}`
6. Repeat for each template/partial pair listed above

**Important:** MiniJinja `{% include %}` inherits the parent template context, so the loop variable is accessible inside the partial. Confirm variable name matches between loop and partial before replacing.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/agents.html` | Replace loop body with `{% include "partials/agent_card.html" %}` |
| `crates/agentos-web/src/templates/tasks.html` | Replace loop body with `{% include "partials/task_row.html" %}` |
| `crates/agentos-web/src/templates/tools.html` | Replace loop body with `{% include "partials/tool_card.html" %}` |
| `crates/agentos-web/src/templates/secrets.html` | Replace loop body with `{% include "partials/secret_row.html" %}` |
| `crates/agentos-web/src/templates/pipelines.html` | Replace loop body with `{% include "partials/pipeline_row.html" %}` |
| `crates/agentos-web/src/templates/audit.html` | Replace loop body with `{% include "partials/log_line.html" %}` |

## Dependencies

None — partials already exist and are loaded by `templates.rs`.

## Test Plan

- `cargo build -p agentos-web` — must compile with no errors
- `cargo test -p agentos-web` — must pass, especially any template rendering tests in `tests/xss_tests.rs`
- Manual: render each full-page template and verify the output contains the expected HTML from the included partials

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
# Each template should now contain the include directive:
grep -n 'include "partials/' crates/agentos-web/src/templates/agents.html
grep -n 'include "partials/' crates/agentos-web/src/templates/tasks.html
grep -n 'include "partials/' crates/agentos-web/src/templates/tools.html
```

## Related

- [[WebUI Security Fixes Plan]] — master plan
- [[01-quick-wins]] — original phase spec (this is the remaining item)
- [[audit_report]] — GAP-C01, GAP-L01
