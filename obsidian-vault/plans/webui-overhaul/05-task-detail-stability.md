---
title: Phase 05 — Task Detail Page Stability
tags:
  - webui
  - tasks
  - sse
  - performance
  - phase-05
date: 2026-04-11
status: planned
effort: 1.5d
priority: high
---

# Phase 05 — Task Detail Page Stability

> Stop the task detail page from freezing on long-running tasks. Add a server-side max-stream-lifetime, a client-side ring buffer that caps log lines at 5,000, idle slowdown when no new entries arrive, an automatic resume on tab focus, and a "view raw" link for the context window so the JSON is no longer rendered inline.

---

## Why this phase

User-reported: "when you are on the task detail page sometimes the whole page will freeze for some reason which won't even reload."

Three contributing factors identified in the audit:

1. **Unbounded DOM growth.** [task_detail.html:120](crates/agentos-web/src/templates/task_detail.html#L120) appends a `<div>` per audit log line with no cap. A long-running task that emits 50,000 audit lines results in 50,000 nodes plus their layout state. Browsers freeze around 10–20K nodes on low-end machines.
2. **Unbounded stream lifetime.** [tasks.rs:350](crates/agentos-web/src/handlers/tasks.rs#L350) polls audit log every 1s and only terminates when the task reaches a terminal state. A `Waiting` task waiting on user input never terminates → stream never closes → DOM keeps growing forever.
3. **Heavy context window inline rendering.** The context window section renders `<pre>{{ msg.content }}</pre>` for each message. If a payload is several KB of escaped JSON × N messages, the layout reflow on `<details>` toggle is significant. Combined with the log terminal growing under it, the browser's main thread stalls.

This phase fixes all three.

## Current → Target State

| Concern | Current | Target |
|---------|---------|--------|
| Log line cap | None | 5,000 visible lines via ring buffer; "(N earlier hidden)" banner |
| Server stream max lifetime | Until task terminal | 30 minutes hard cap; emits a `closed` event with reason |
| Server poll interval | Always 1 s | 1 s while active; back off to 10 s after 2 min idle |
| Stream resume after refresh / tab blur | None | `Last-Event-ID` header; server resumes from that audit ID |
| Context window rendering | `<pre>` inline with full payload | Summary + "View raw" link to `/api/tasks/{id}/context/{idx}` |
| Page interaction during stream | Often unresponsive | Auto-scroll respects user's scroll position; "↓ N new" pin button when scrolled away |
| Cancel / retry | Cancel button only when state is Running/Queued/Waiting | Same, plus "Pause Stream" button always available |
| Findings panel | Working | Same — Phase 05 doesn't touch this |

## Detailed subtasks

### 1. Server-side max stream lifetime + idle backoff

```rust
// crates/agentos-web/src/handlers/tasks.rs
const MAX_STREAM_LIFETIME: Duration = Duration::from_secs(30 * 60);  // 30 min
const IDLE_THRESHOLD: Duration = Duration::from_secs(120);            // 2 min
const ACTIVE_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_INTERVAL: Duration = Duration::from_secs(10);

pub async fn log_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let task_id = match id.parse::<TaskID>() {
        Ok(t) => t,
        Err(_) => return Sse::new(stream::empty()),
    };

    // Resume support: client passes last seen audit ID via Last-Event-ID header
    let resume_from: i64 = headers.get(axum::http::header::LAST_EVENT_ID)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let started_at = Instant::now();

    let stream = stream::unfold(
        (Some(resume_from), Instant::now(), ACTIVE_INTERVAL),
        move |(state_opt, last_activity, interval)| async move {
            let last_seen_id = state_opt?;

            // Hard timeout
            if started_at.elapsed() > MAX_STREAM_LIFETIME {
                let event = Event::default()
                    .event("closed")
                    .data(r#"{"reason":"max_lifetime"}"#);
                return Some((Ok(event), None));
            }

            tokio::time::sleep(interval).await;

            let entries = fetch_audit_entries_since(&state.kernel, &task_id, last_seen_id, 100).await;
            let task_state = state.kernel.scheduler.get_task(&task_id).await.map(|t| t.state);
            let is_terminal = matches!(
                task_state,
                Some(TaskState::Complete | TaskState::Failed | TaskState::Cancelled)
            );

            let mut events = Vec::new();
            let max_id = entries.iter().map(|(id, _)| *id).max().unwrap_or(last_seen_id);

            for (id, entry) in &entries {
                if entry.event_type == AuditEventType::TestFindingCaptured {
                    events.push(Ok(Event::default()
                        .id(id.to_string())
                        .event("finding")
                        .data(entry.details.to_string())));
                } else {
                    let line = format!("[{}] {} - {}",
                        entry.timestamp.format("%H:%M:%S"),
                        humanize_event_type(entry.event_type),
                        summarize_details(&entry.details));
                    events.push(Ok(Event::default().id(id.to_string()).data(line)));
                }
            }

            // Determine next state
            let now = Instant::now();
            let new_last_activity = if !entries.is_empty() { now } else { last_activity };
            let new_interval = if now.duration_since(new_last_activity) > IDLE_THRESHOLD {
                IDLE_INTERVAL
            } else {
                ACTIVE_INTERVAL
            };

            if is_terminal && entries.is_empty() {
                events.push(Ok(Event::default().event("done").data("")));
                return Some((futures::stream::iter(events), None));
            }

            Some((futures::stream::iter(events), Some((Some(max_id), new_last_activity, new_interval))))
        },
    )
    .flatten();

    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

(Pseudo-Rust — the actual `unfold` signature needs to yield `Result<Event, Infallible>` per item; either flatten via `futures::stream::iter` or refactor to yield one event at a time and store a `Vec` in the state.)

### 2. Client-side ring buffer

Replace [task_detail.html:120-130](crates/agentos-web/src/templates/task_detail.html#L120) with:

```javascript
const MAX_LINES = 5000;
const TRIM_BATCH = 200;
let droppedLines = 0;
let isPinnedToBottom = true;

const droppedBanner = document.createElement('div');
droppedBanner.className = 'log-dropped-banner';
droppedBanner.hidden = true;
terminal.parentNode.insertBefore(droppedBanner, terminal);

function appendLine(text) {
    if (!text.trim()) return;
    const line = document.createElement('div');
    line.className = 'log-line ' + classifyLine(text);
    line.textContent = text;
    terminal.appendChild(line);

    while (terminal.childElementCount > MAX_LINES) {
        for (let i = 0; i < TRIM_BATCH && terminal.firstElementChild; i++) {
            terminal.firstElementChild.remove();
            droppedLines++;
        }
    }
    if (droppedLines > 0) {
        droppedBanner.textContent = `(${droppedLines.toLocaleString()} earlier lines hidden — ring buffer cap)`;
        droppedBanner.hidden = false;
    }

    if (isPinnedToBottom && autoscrollCheck.checked) {
        terminal.scrollTop = terminal.scrollHeight;
    } else {
        bumpPendingCount();
    }
}

terminal.addEventListener('scroll', function () {
    isPinnedToBottom =
        terminal.scrollTop + terminal.clientHeight >= terminal.scrollHeight - 4;
    if (isPinnedToBottom) clearPendingCount();
});

const pendingPin = document.createElement('button');
pendingPin.className = 'log-pending-pin';
pendingPin.hidden = true;
pendingPin.addEventListener('click', function () {
    terminal.scrollTop = terminal.scrollHeight;
    isPinnedToBottom = true;
    clearPendingCount();
});
terminal.parentNode.appendChild(pendingPin);

let pendingCount = 0;
function bumpPendingCount() {
    pendingCount++;
    pendingPin.hidden = false;
    pendingPin.textContent = `↓ ${pendingCount} new`;
}
function clearPendingCount() {
    pendingCount = 0;
    pendingPin.hidden = true;
}
```

### 3. Resume + reconnect on tab focus

```javascript
let lastEventId = 0;
let src = null;

function connect() {
    src = new EventSource('/tasks/{{ task_id }}/logs/stream');

    src.addEventListener('message', function (e) {
        if (e.lastEventId) lastEventId = parseInt(e.lastEventId, 10) || lastEventId;
        appendLine(e.data);
    });
    src.addEventListener('finding', function (e) {
        if (e.lastEventId) lastEventId = parseInt(e.lastEventId, 10) || lastEventId;
        try { addFinding(JSON.parse(e.data)); }
        catch (err) { appendLine('[finding parse error]'); }
    });
    src.addEventListener('done', function () {
        setStatus('complete', 'badge-complete');
        appendDivider('─── stream closed ───');
        src.close();
    });
    src.addEventListener('closed', function (e) {
        try {
            const reason = JSON.parse(e.data).reason;
            setStatus('closed (' + reason + ')', 'badge-warning');
        } catch (_) { setStatus('closed', 'badge-warning'); }
        appendDivider('─── stream closed by server (refresh to reconnect) ───');
        src.close();
    });
    src.onerror = function () {
        if (src.readyState === EventSource.CLOSED) return;
        setStatus('disconnected', 'badge-error');
    };
}
connect();

document.addEventListener('visibilitychange', function () {
    if (document.visibilityState === 'visible' && (!src || src.readyState === EventSource.CLOSED)) {
        // Browser auto-reconnects via Last-Event-ID since we set `Event::id()` on every event
        connect();
    }
});

window.addEventListener('beforeunload', function () {
    if (src) src.close();
});
```

### 4. Context window — replace inline JSON with summary + "view raw" link

In [task_detail.html:42-55](crates/agentos-web/src/templates/task_detail.html#L42-L55):

```html
{% if history %}
<h2>Context Window <small class="muted">({{ history|length }} messages)</small></h2>
<div class="context-window">
    {% for msg in history %}
    <details class="context-entry">
        <summary class="context-summary">
            <span class="role-badge role-{{ msg.role|lower }}">{{ msg.role|human_role }}</span>
            <span class="muted context-preview">{{ msg.payload|pretty_json|truncate(80) }}</span>
            <span class="muted">{{ msg.timestamp|relative_time }}</span>
        </summary>
        <pre class="context-content"><code class="language-json">{{ msg.payload|pretty_json }}</code></pre>
        <div class="context-footer">
            <a href="/api/tasks/{{ task_id }}/context/{{ loop.index0 }}/raw" target="_blank">View raw payload →</a>
        </div>
    </details>
    {% endfor %}
</div>
{% endif %}
```

The `pretty_json` filter (added in Phase 03) caps input at 256 KB; anything larger gets truncated with a "view raw" link to a new endpoint:

```rust
// crates/agentos-web/src/router.rs
.route("/api/tasks/{id}/context/{idx}/raw", get(tasks::context_raw))

// crates/agentos-web/src/handlers/tasks.rs
pub async fn context_raw(
    State(state): State<AppState>,
    Path((id, idx)): Path<(String, usize)>,
) -> Response {
    let task_id = id.parse::<TaskID>().ok();
    let task = match task_id { Some(tid) => state.kernel.scheduler.get_task(&tid).await, None => None };
    let task = match task { Some(t) => t, None => return StatusCode::NOT_FOUND.into_response() };
    let msg = match task.history.get(idx) { Some(m) => m, None => return StatusCode::NOT_FOUND.into_response() };
    let body = serde_json::to_string_pretty(&msg.payload).unwrap_or_default();
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}
```

### 5. Pause Stream button

Add a button next to the Auto-scroll toggle:

```html
<button type="button" class="outline secondary btn-sm" id="pause-stream">Pause Stream</button>
```

```javascript
let paused = false;
document.getElementById('pause-stream').addEventListener('click', function () {
    paused = !paused;
    this.textContent = paused ? 'Resume Stream' : 'Pause Stream';
    if (paused && src) src.close();
    else connect();
});
```

When paused, the EventSource is closed but `lastEventId` is preserved, so resuming reconnects exactly where we left off.

### 6. Limit context window initial render to N most-recent messages

If `history.len() > 50`, only render the last 50 in the template and add a "Show all N messages →" link to a paginated raw view. This prevents the very first paint from stalling on tasks with thousands of context messages.

```html
{% set total = history|length %}
{% if total > 50 %}
<div class="context-pagination muted">
    Showing the most recent 50 of {{ total }} messages.
    <a href="/api/tasks/{{ task_id }}/context/all">Show all →</a>
</div>
{% endif %}
{% for msg in history[-50:] %}
    ...
{% endfor %}
```

## Files changed

| File | Change |
|------|--------|
| [crates/agentos-web/src/handlers/tasks.rs](crates/agentos-web/src/handlers/tasks.rs) | `log_stream` rewritten with lifetime + idle backoff + event IDs; new `context_raw` handler |
| [crates/agentos-web/src/templates/task_detail.html](crates/agentos-web/src/templates/task_detail.html) | Ring buffer JS, dropped banner, pending pin, pause button, context truncation, view-raw links |
| [crates/agentos-web/src/router.rs](crates/agentos-web/src/router.rs) | New `/api/tasks/{id}/context/{idx}/raw` route |
| [crates/agentos-web/static/css/app.css](crates/agentos-web/static/css/app.css) | Styles for `.log-dropped-banner`, `.log-pending-pin` |

## Dependencies

- Soft-requires: [[03-template-rendering-fixes]] for `human_role` / `pretty_json` / `relative_time` filters
- Independent of: 01, 02, 04, 06, 07

## Test plan

1. **Unit: server stream lifetime**
   - Spawn the stream against a mock task that never terminates. Use `tokio::time::pause()` and advance 31 minutes. Assert a `closed` event is emitted with reason `max_lifetime`.
2. **Unit: idle backoff**
   - Spawn the stream against a task with no audit entries. Advance 130s. Assert subsequent polls happen at 10s intervals.
3. **Unit: resume from Last-Event-ID**
   - Spawn the stream with `Last-Event-ID: 100`. Assert the SQL `query_since_for_task` is called with `100` as the cursor.
4. **Integration: context_raw**
   - Create a task with 3 history messages. GET `/api/tasks/{id}/context/1/raw`. Assert 200, JSON body matches `task.history[1].payload`.
5. **Manual: stress test**
   - Run a task that emits 20,000 audit entries (write a debug tool that loops). Open the task detail page. Expected: page stays interactive throughout, log terminal caps at 5,000 visible lines, banner shows "15,000 earlier lines hidden", pending pin shows new lines when scrolled away.
6. **Manual: refresh during streaming**
   - Open the task page during a 5-min run. Refresh after 30 sec. Expected: only new audit entries (since last seen ID) arrive, no duplicates.

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web handlers::tasks::log_stream
cargo clippy -p agentos-web -- -D warnings
cargo fmt --all -- --check

# Manual stress test
docker compose restart agentos-kernel
agentos task run --agent test --prompt "(use a debug tool that emits 20000 audit lines)"
# Open task detail in browser, verify stays responsive
```

## Related

- [[WebUI Overhaul Plan]]
- [[03-template-rendering-fixes]]
