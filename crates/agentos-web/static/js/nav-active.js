// Highlights the active sidebar nav link based on the current URL path
// and auto-opens the collapsible section that contains it.
(function () {
    window.addEventListener('DOMContentLoaded', function() {
        requestAnimationFrame(function () {
            var path = window.location.pathname;
            document.querySelectorAll('[data-nav]').forEach(function (a) {
                var href = a.getAttribute('data-nav');
                var active = href === '/' ? path === '/' : path === href || path.startsWith(href + '/');
                if (active) {
                    a.classList.add('nav-active');
                    a.setAttribute('aria-current', 'page');
                    // Auto-open the collapsible section containing the active link.
                    var section = a.closest('details.nav-section');
                    if (section) section.setAttribute('open', '');
                }
            });
        });
    });
}());
