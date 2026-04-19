// Alpine.js component for the notification bell indicator with SSE reconnection logic.
function notificationBell() {
    return {
        unreadCount: 0,
        evtSource: null,
        _retries: 0,
        _maxRetries: 5,
        _retryTimer: null,
        init() {
            var self = this;
            // Fetch current unread count immediately so the bell is accurate on load.
            fetch('/notifications/unread-count')
                .then(function (r) { return r.ok ? r.json() : null; })
                .then(function (data) { if (data) { self.unreadCount = data.count; } })
                .catch(function () {});
            self._connect();
        },
        _connect() {
            var self = this;
            if (self.evtSource) {
                self.evtSource.close();
                self.evtSource = null;
            }
            self.evtSource = new EventSource('/notifications/stream');
            self.evtSource.addEventListener('notification-new', function (e) {
                self.unreadCount += 1;
                try {
                    var data = JSON.parse(e.data);
                    window.dispatchEvent(new CustomEvent('showToast', {
                        detail: {
                            message: data.subject || 'New notification',
                            type: data.priority === 'critical' ? 'error'
                                : data.priority === 'urgent' ? 'warning'
                                : 'info'
                        }
                    }));
                } catch (_) {}
            });
            self.evtSource.addEventListener('notification-reload', function () {
                fetch('/notifications/unread-count')
                    .then(function (r) { return r.ok ? r.json() : null; })
                    .then(function (data) { if (data) { self.unreadCount = data.count; } })
                    .catch(function () {});
            });
            self.evtSource.onopen = function () {
                self._retries = 0;
            };
            self.evtSource.onerror = function () {
                // Close immediately to stop the browser's built-in auto-reconnect
                // which can saturate connection limits and freeze the page.
                if (self.evtSource) {
                    self.evtSource.close();
                    self.evtSource = null;
                }
                if (self._retries < self._maxRetries) {
                    self._retries++;
                    var delay = Math.min(1000 * Math.pow(2, self._retries), 30000);
                    self._retryTimer = setTimeout(function () {
                        self._retryTimer = null;
                        self._connect();
                    }, delay);
                }
            };
        },
        destroy() {
            if (this._retryTimer) {
                clearTimeout(this._retryTimer);
                this._retryTimer = null;
            }
            if (this.evtSource) {
                this.evtSource.close();
                this.evtSource = null;
            }
        }
    };
}
