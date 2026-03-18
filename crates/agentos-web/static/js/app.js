// AgentOS Web UI — client-side utilities

// Theme toggle (Alpine.js component)
function themeToggle() {
    return {
        dark: localStorage.getItem('agentos-theme') === 'dark',
        toggle: function() {
            this.dark = !this.dark;
            document.documentElement.setAttribute(
                'data-theme', this.dark ? 'dark' : 'light'
            );
            localStorage.setItem('agentos-theme', this.dark ? 'dark' : 'light');
        },
        init: function() {
            if (this.dark) {
                document.documentElement.setAttribute('data-theme', 'dark');
            }
        }
    };
}

// SSE connection lifecycle: close connections when the tab is hidden to save resources.
// The HTMX SSE extension stores the EventSource on the element as `htmx-internal-data`.
document.addEventListener('visibilitychange', function () {
    if (document.hidden) {
        document.querySelectorAll('[sse-connect]').forEach(function (el) {
            var internalData = el['htmx-internal-data'];
            if (internalData && internalData.sseEventSource) {
                internalData.sseEventSource.close();
            }
        });
    }
});

// Keyboard shortcuts (Alpine.js component)
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
                        s: '/secrets', p: '/pipelines', l: '/audit'
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

// Toast notification store (Alpine.js component)
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
