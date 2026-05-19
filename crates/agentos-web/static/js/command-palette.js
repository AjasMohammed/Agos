// AgentOS — Command palette (Alpine.js component)
// Cmd/Ctrl+K to open. Fuzzy navigation for any sidebar page.
function commandPalette() {
    var routes = [
        { section: 'Home',         label: 'Dashboard',                href: '/' },
        { section: 'Operations',   label: 'Agents',                   href: '/agents' },
        { section: 'Operations',   label: 'Tasks',                    href: '/tasks' },
        { section: 'Operations',   label: 'Running tasks',            href: '/tasks?status=running' },
        { section: 'Operations',   label: 'Failed tasks',             href: '/tasks?status=failed' },
        { section: 'Operations',   label: 'Chat',                     href: '/chat' },
        { section: 'Operations',   label: 'Agent Chat (A2A)',         href: '/agent-chat' },
        { section: 'Operations',   label: 'Files',                    href: '/files' },
        { section: 'Operations',   label: 'Workflows',                href: '/workflows' },
        { section: 'Operations',   label: 'Pipelines',                href: '/pipelines' },
        { section: 'Operations',   label: 'Escalations · Approvals',  href: '/escalations' },
        { section: 'Operations',   label: 'Preference proposals',     href: '/prefs' },
        { section: 'Capabilities', label: 'Tools',                    href: '/tools' },
        { section: 'Capabilities', label: 'Agent Manual',             href: '/manual' },
        { section: 'Capabilities', label: 'Marketplace',              href: '/marketplace' },
        { section: 'Capabilities', label: 'Schedules',                href: '/schedules' },
        { section: 'Capabilities', label: 'Roles',                    href: '/roles' },
        { section: 'Capabilities', label: 'Teams',                    href: '/teams' },
        { section: 'Capabilities', label: 'Scratchpad',               href: '/scratchpad' },
        { section: 'Integrations', label: 'Plugins',                  href: '/plugins' },
        { section: 'Integrations', label: 'Channels',                 href: '/channels' },
        { section: 'Integrations', label: 'MCP Servers',              href: '/mcp' },
        { section: 'Integrations', label: 'Webhooks',                 href: '/webhooks' },
        { section: 'Integrations', label: 'Connectors',               href: '/connectors' },
        { section: 'Integrations', label: 'A2A',                      href: '/a2a' },
        { section: 'System',       label: 'Audit Log',                href: '/audit' },
        { section: 'System',       label: 'Costs',                    href: '/costs' },
        { section: 'System',       label: 'Secrets',                  href: '/secrets' },
        { section: 'System',       label: 'Config',                   href: '/config' },
        { section: 'System',       label: 'Notifications',            href: '/notifications' },
        { section: 'System',       label: 'Doctor · health checks',   href: '/doctor' },
        { section: 'System',       label: 'Events',                   href: '/events' },
        { section: 'System',       label: 'Logs',                     href: '/logs' },
        { section: 'System',       label: 'Resources',                href: '/resources' },
        { section: 'System',       label: 'HAL',                      href: '/hal' },
        { section: 'Action',       label: 'New chat',                 href: '/chat' },
        { section: 'Action',       label: 'Connect new agent',        href: '/agents' },
        { section: 'Action',       label: 'Show keyboard shortcuts',  href: '#shortcuts' },
    ];

    function score(needle, hay) {
        if (!needle) return 1;
        var n = needle.toLowerCase();
        var h = hay.toLowerCase();
        if (h === n) return 1000;
        if (h.startsWith(n)) return 500;
        if (h.indexOf(' ' + n) !== -1) return 300;
        if (h.indexOf(n) !== -1) return 200;
        // Subsequence match (each char in order)
        var i = 0, j = 0, hits = 0;
        while (i < n.length && j < h.length) {
            if (n[i] === h[j]) { hits++; i++; }
            j++;
        }
        return (i === n.length) ? hits : 0;
    }

    return {
        show: false,
        query: '',
        active: 0,
        _keyHandler: null,
        init: function() {
            var self = this;
            self._keyHandler = function(e) {
                var key = e.key && e.key.toLowerCase();
                if ((e.metaKey || e.ctrlKey) && key === 'k') {
                    e.preventDefault();
                    self.toggle();
                }
            };
            window.addEventListener('keydown', self._keyHandler);
        },
        destroy: function() {
            if (this._keyHandler) {
                window.removeEventListener('keydown', this._keyHandler);
                this._keyHandler = null;
            }
        },
        toggle: function() { this.show ? this.close() : this.open(); },
        open: function() {
            this.show = true;
            this.query = '';
            this.active = 0;
            var self = this;
            setTimeout(function() {
                if (self.$refs.input) self.$refs.input.focus();
            }, 20);
        },
        close: function() { this.show = false; },
        filtered: function() {
            var q = this.query.trim();
            var scored = routes.map(function(r) {
                var s = score(q, r.label) + 0.1 * score(q, r.section);
                return { r: r, s: s };
            }).filter(function(x) { return x.s > 0; });
            scored.sort(function(a, b) { return b.s - a.s; });
            var list = scored.slice(0, 12).map(function(x) { return x.r; });
            if (this.active >= list.length) this.active = Math.max(0, list.length - 1);
            return list;
        },
        moveDown: function() {
            var max = this.filtered().length - 1;
            this.active = Math.min(this.active + 1, max);
        },
        moveUp: function() {
            this.active = Math.max(this.active - 1, 0);
        },
        activate: function() {
            var list = this.filtered();
            var item = list[this.active];
            if (item) this.go(item.href);
        },
        go: function(href) {
            this.close();
            if (href === '#shortcuts') {
                window.dispatchEvent(new CustomEvent('show-shortcuts'));
                return;
            }
            window.location.href = href;
        }
    };
}
