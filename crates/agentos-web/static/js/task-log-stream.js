// Task detail page — live log stream and tester findings panel.
// Reads task ID from data-task-id attribute on the log terminal element.
(function () {
    var terminal = document.getElementById('log-terminal');
    if (!terminal) return;
    var taskId = terminal.getAttribute('data-task-id');
    if (!taskId) return;

    var statusBadge = document.getElementById('stream-status');
    var autoscrollCheck = document.getElementById('autoscroll');
    var findingsList = document.getElementById('findings-list');
    var findingsCount = document.getElementById('findings-count');
    var findingsEmpty = document.getElementById('findings-empty');
    var count = 0;
    var MAX_LOG_LINES = 5000;
    var droppedLogLines = 0;
    var droppedIndicator = null;

    window.clearLogs = function () {
        terminal.innerHTML = '';
        droppedLogLines = 0;
        droppedIndicator = null;
    };
    window.clearFindings = function () {
        findingsList.innerHTML = '';
        count = 0;
        findingsCount.textContent = '0';
        var empty = document.createElement('p');
        empty.className = 'findings-empty muted';
        empty.id = 'findings-empty';
        empty.textContent = 'No findings yet — findings emitted by the agent will appear here in real-time.';
        findingsList.appendChild(empty);
        findingsEmpty = empty;
    };

    var scrollRafId = null;
    function scheduleScroll() {
        if (scrollRafId) return;
        scrollRafId = requestAnimationFrame(function () {
            scrollRafId = null;
            if (autoscrollCheck.checked) {
                terminal.scrollTop = terminal.scrollHeight;
            }
        });
    }

    var reError = /error|failed|critical|panic/i;
    var reWarn = /warn/i;
    var reSuccess = /complete|success|\bok\b/i;
    var reTool = /toolcall|tool/i;

    function classifyLine(text) {
        if (reError.test(text)) return 'log-error';
        if (reWarn.test(text)) return 'log-warn';
        if (reSuccess.test(text)) return 'log-success';
        if (reTool.test(text)) return 'log-tool';
        return '';
    }

    // Batch-append lines into a DocumentFragment, then trim excess in one pass.
    function appendLines(texts) {
        var frag = document.createDocumentFragment();
        var added = 0;
        for (var i = 0; i < texts.length; i++) {
            var text = texts[i];
            if (!text.trim()) continue;
            if (text.length > 4000) {
                text = text.slice(0, 4000) + '… [truncated]';
            }
            var line = document.createElement('div');
            line.className = 'log-line';
            var cls = classifyLine(text);
            if (cls) line.classList.add(cls);
            line.textContent = text;
            frag.appendChild(line);
            added++;
        }
        if (added === 0) return;
        terminal.appendChild(frag);

        // Trim excess in one batch instead of one-by-one
        var excess = terminal.childElementCount - MAX_LOG_LINES;
        if (excess > 0) {
            for (var j = 0; j < excess; j++) {
                terminal.removeChild(terminal.firstElementChild);
            }
            droppedLogLines += excess;
            // Re-create indicator if it was removed by the trim loop above
            if (!droppedIndicator || !droppedIndicator.parentNode) {
                droppedIndicator = document.createElement('div');
                droppedIndicator.className = 'log-divider';
                terminal.insertBefore(droppedIndicator, terminal.firstChild);
            }
            droppedIndicator.textContent = '─── older lines hidden: ' + droppedLogLines + ' ───';
        }
        scheduleScroll();
    }

    // Single-line append kept for dividers
    function appendLine(text) {
        appendLines([text]);
    }

    function appendDivider(text) {
        var line = document.createElement('div');
        line.className = 'log-divider';
        line.textContent = text;
        terminal.appendChild(line);
        scheduleScroll();
    }

    function setStatus(label, badgeClass) {
        statusBadge.textContent = label;
        statusBadge.className = 'badge ' + badgeClass;
    }

    var SEVERITY_ICON = { error: '✕', warning: '⚠', warn: '⚠', info: 'ℹ' };
    var CATEGORY_LABEL = { usability: 'Usability', correctness: 'Correctness', ergonomics: 'Ergonomics', security: 'Security', performance: 'Performance' };

    function addFinding(f) {
        if (findingsEmpty) {
            findingsEmpty.remove();
            findingsEmpty = null;
        }
        count++;
        findingsCount.textContent = count;

        var sev = (f.severity || 'info').toLowerCase();
        var cat = (f.category || '').toLowerCase();

        var card = document.createElement('article');
        card.className = 'finding-card finding-' + sev;

        var header = document.createElement('header');
        header.className = 'finding-card-header';

        var sevSpan = document.createElement('span');
        sevSpan.className = 'finding-severity finding-severity-' + sev;
        sevSpan.textContent = (SEVERITY_ICON[sev] || '·') + ' ' + sev;

        var catSpan = document.createElement('span');
        catSpan.className = 'finding-category';
        catSpan.textContent = CATEGORY_LABEL[cat] || cat;

        header.appendChild(sevSpan);
        header.appendChild(catSpan);
        card.appendChild(header);

        var obs = document.createElement('p');
        obs.className = 'finding-observation';
        obs.textContent = f.observation || '';
        card.appendChild(obs);

        if (f.suggestion) {
            var sug = document.createElement('small');
            sug.className = 'finding-suggestion muted';
            sug.textContent = '→ ' + f.suggestion;
            card.appendChild(sug);
        }

        if (f.context) {
            var ctx = document.createElement('small');
            ctx.className = 'finding-context muted';
            ctx.textContent = f.context;
            card.appendChild(ctx);
        }

        findingsList.prepend(card);
    }

    var src = null;
    var paused = false;
    var finished = false;   // true once stream sends "done" — never reconnect
    var errorRetries = 0;
    var maxErrorRetries = 5;
    var retryTimer = null;
    var pauseBtn = document.getElementById('pause-stream');

    function connect() {
        if (finished) return;
        if (src) { src.close(); src = null; }
        src = new EventSource('/tasks/' + encodeURIComponent(taskId) + '/logs/stream');

        src.onopen = function () {
            errorRetries = 0;
            setStatus('streaming', 'badge-running');
        };

        src.onmessage = function (event) {
            appendLines(event.data.split('\n'));
        };

        src.addEventListener('finding', function (event) {
            try {
                var f = JSON.parse(event.data);
                addFinding(f);
            } catch (e) {
                appendLine('[finding parse error] ' + event.data);
            }
        });

        src.addEventListener('done', function () {
            finished = true;
            setStatus('complete', 'badge-complete');
            appendDivider('─── stream closed ───');
            src.close();
            src = null;
        });

        src.onerror = function () {
            // Close immediately to stop browser auto-reconnect from saturating connections.
            if (src) { src.close(); src = null; }
            setStatus('disconnected', 'badge-error');
            if (errorRetries < maxErrorRetries) {
                errorRetries++;
                var delay = Math.min(1000 * Math.pow(2, errorRetries), 30000);
                retryTimer = setTimeout(function () {
                    retryTimer = null;
                    appendDivider('─── reconnecting (' + errorRetries + '/' + maxErrorRetries + ') ───');
                    connect();
                }, delay);
            } else {
                appendDivider('─── connection lost — click Pause/Resume to retry ───');
            }
        };
    }

    connect();

    if (pauseBtn) {
        pauseBtn.addEventListener('click', function () {
            paused = !paused;
            this.textContent = paused ? 'Resume Stream' : 'Pause Stream';
            if (paused && src) {
                src.close();
                src = null;
                if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; }
                setStatus('paused', 'badge-warning');
            } else if (!paused && !finished) {
                errorRetries = 0;
                connect();
            }
        });
    }

    document.addEventListener('visibilitychange', function () {
        if (document.visibilityState === 'visible' && !paused && !finished && !src) {
            errorRetries = 0;
            connect();
        }
        // Pause when tab is hidden to avoid saturating connections from idle background tabs.
        if (document.hidden) {
            if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; }
            if (src) { src.close(); src = null; }
        }
    });

    // Close SSE immediately when the user navigates away via a link click
    // (fires before beforeunload, eliminating the stale connection during page transition).
    document.addEventListener('click', function (e) {
        var link = e.target.closest('a[href]');
        if (!link) return;
        if (e.defaultPrevented || e.button !== 0) return;
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
        if (link.target && link.target !== '_self') return;
        var url;
        try { url = new URL(link.href, window.location.href); } catch (_) { return; }
        if (url.origin !== window.location.origin) return;
        // Same-origin navigation: immediately tear down the SSE.
        if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; }
        if (src) { src.close(); src = null; }
    });

    window.addEventListener('beforeunload', function () {
        if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; }
        if (src) { src.close(); }
    });
}());
