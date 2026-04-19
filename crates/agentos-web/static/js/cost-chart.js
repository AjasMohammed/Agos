// Alpine.js component for the cost dashboard bar chart.
// Reads chart data from the #cost-chart-data JSON script element injected by the template.
function costChart() {
    return {
        agents: [],
        colors: [
            'var(--pico-color-blue-500, #3b82f6)',
            'var(--pico-color-green-500, #22c55e)',
            'var(--pico-color-orange-500, #f97316)',
            'var(--pico-color-purple-500, #a855f7)',
            'var(--pico-color-red-500, #ef4444)',
            'var(--pico-color-cyan-500, #06b6d4)'
        ],
        init: function() {
            var el = document.getElementById('cost-chart-data');
            var data = [];
            if (el) {
                try { data = JSON.parse(el.textContent); } catch (_) {}
            }
            var maxCost = Math.max.apply(null, data.map(function(d) { return d.raw; }));
            if (maxCost <= 0) maxCost = 1;
            this.agents = data.map(function(d) {
                return {
                    name: d.name,
                    cost: d.cost,
                    pct: ((d.raw / maxCost) * 100).toFixed(1)
                };
            });
        }
    };
}
