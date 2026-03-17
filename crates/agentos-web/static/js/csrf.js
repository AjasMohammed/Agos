// Inject the per-session CSRF token into every HTMX request.
document.addEventListener('htmx:configRequest', function (event) {
    var meta = document.querySelector('meta[name="csrf-token"]');
    if (meta) {
        event.detail.headers['X-CSRF-Token'] = meta.content;
    }
});
