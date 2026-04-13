// AgentOS Web UI — client-side utilities

// ── SSE Connection Status (Alpine.js component) ───────────
// Monitors SSE connections on the page and exposes state for the topbar indicator.
// Note: relies on htmx-internal-data (HTMX internal API) — pin HTMX version.
function sseStatus() {
    return {
        state: 'disconnected',
        label: 'Offline',
        _intervalId: null,
        init: function() {
            var self = this;
            function check() {
                var els = document.querySelectorAll('[sse-connect]');
                if (els.length === 0) {
                    self.state = 'disconnected';
                    self.label = 'No stream';
                    return;
                }
                var connected = 0;
                var reconnecting = 0;
                els.forEach(function(el) {
                    var d = el['htmx-internal-data'];
                    if (d && d.sseEventSource) {
                        var rs = d.sseEventSource.readyState;
                        if (rs === EventSource.OPEN) connected++;
                        else if (rs === EventSource.CONNECTING) reconnecting++;
                    }
                });
                if (connected > 0) {
                    self.state = 'connected';
                    self.label = 'Live';
                } else if (reconnecting > 0) {
                    self.state = 'reconnecting';
                    self.label = 'Reconnecting';
                } else {
                    self.state = 'disconnected';
                    self.label = 'Offline';
                }
            }
            check();
            self._intervalId = setInterval(check, 2000);
        },
        destroy: function() {
            if (this._intervalId) {
                clearInterval(this._intervalId);
                this._intervalId = null;
            }
        }
    };
}

// ── SSE Update Flash Animation ────────────────────────────
// When HTMX swaps in new SSE content, briefly flash the target to give visual feedback.
document.body.addEventListener('htmx:sseMessage', function(event) {
    var target = event.detail.elt;
    if (!target) return;

    // Add flash class
    target.classList.add('sse-flash');

    // Animate stat values that changed
    var newValues = target.querySelectorAll('.stat-value[data-animate]');
    newValues.forEach(function(el) {
        var key = el.closest('.stat-card') ? el.textContent.trim() : null;
        if (key) {
            var prev = el.getAttribute('data-prev');
            if (prev !== null && prev !== key) {
                el.classList.add('value-changed');
                setTimeout(function() { el.classList.remove('value-changed'); }, 350);
            }
            el.setAttribute('data-prev', key);
        }
    });

    // Remove flash after animation
    setTimeout(function() { target.classList.remove('sse-flash'); }, 650);
});

// ── SSE connection lifecycle ──────────────────────────────
// Close connections when the tab is hidden to save resources,
// and re-establish them when the tab becomes visible again.
document.addEventListener('visibilitychange', function () {
    if (document.hidden) {
        document.querySelectorAll('[sse-connect]').forEach(function (el) {
            var internalData = el['htmx-internal-data'];
            if (internalData && internalData.sseEventSource) {
                internalData.sseEventSource.close();
            }
        });
    } else {
        // Tab is visible again — ask HTMX to re-process SSE elements
        // so it re-creates the closed EventSource connections.
        document.querySelectorAll('[sse-connect]').forEach(function (el) {
            htmx.process(el);
        });
    }
});

// ── Keyboard shortcuts (Alpine.js component) ──────────────
function keyboardNav() {
    return {
        awaitingGoto: false,
        init: function() {
            var self = this;
            document.addEventListener('keydown', function(e) {
                var tag = e.target.tagName;
                if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || e.target.isContentEditable) return;

                // Show keyboard shortcut help on '?'
                if (e.key === '?' && !e.ctrlKey && !e.metaKey && !e.altKey) {
                    e.preventDefault();
                    window.dispatchEvent(new CustomEvent('show-shortcuts'));
                    return;
                }

                if (e.key === 'g' && !e.ctrlKey && !e.metaKey && !e.altKey) {
                    self.awaitingGoto = true;
                    setTimeout(function() { self.awaitingGoto = false; }, 1000);
                    return;
                }
                if (self.awaitingGoto) {
                    self.awaitingGoto = false;
                    var routes = {
                        d: '/', a: '/agents', t: '/tasks', o: '/tools',
                        s: '/secrets', p: '/pipelines', l: '/audit',
                        c: '/chat', n: '/notifications'
                    };
                    if (routes[e.key]) {
                        e.preventDefault();
                        window.location.href = routes[e.key];
                    }
                }
            });
        }
    };
}

// ── Toast notification store (Alpine.js component) ────────
function toastStore() {
    return {
        toasts: [],
        addToast: function(detail) {
            var message = typeof detail === 'string' ? detail : (detail.message || '');
            var type = (detail && detail.type) ? detail.type : 'info';
            var id = Date.now() + Math.random();
            this.toasts.push({ id: id, message: message, type: type });
            var self = this;
            var timeout = type === 'error' ? 8000 : 5000;
            setTimeout(function() {
                self.removeToast(id);
            }, timeout);
        },
        removeToast: function(id) {
            this.toasts = this.toasts.filter(function(t) { return t.id !== id; });
        }
    };
}

// Bridge HTMX HX-Trigger "showToast" events to Alpine's custom event system
document.body.addEventListener('showToast', function(event) {
    window.dispatchEvent(new CustomEvent('show-toast', { detail: event.detail }));
});

// Bridge HTMX HX-Trigger "closeAgentModal" to Alpine's custom event for the dialog
document.body.addEventListener('closeAgentModal', function() {
    window.dispatchEvent(new CustomEvent('close-agent-modal'));
});
