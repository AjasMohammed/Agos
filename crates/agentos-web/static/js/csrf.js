// Inject the per-session CSRF token into every HTMX request.
document.addEventListener('htmx:configRequest', function (event) {
    var meta = document.querySelector('meta[name="csrf-token"]');
    if (meta && meta.content) {
        event.detail.headers['X-CSRF-Token'] = meta.content;
    } else {
        console.error('AgentOS: CSRF meta tag missing or empty. This will cause 403 Forbidden errors for state-changing requests (POST, DELETE, etc.).');
    }
});
