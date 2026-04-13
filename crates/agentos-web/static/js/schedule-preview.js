(function () {
    function getCsrfToken() {
        var meta = document.querySelector('meta[name="csrf-token"]');
        return meta ? meta.getAttribute('content') || '' : '';
    }

    function updatePreview(text) {
        var el = document.getElementById('cron-preview');
        if (el) {
            el.textContent = text;
        }
    }

    var input = document.getElementById('schedule-cron-input');
    if (!input) {
        return;
    }

    var timer = null;
    input.addEventListener('input', function () {
        if (timer) {
            clearTimeout(timer);
        }
        timer = setTimeout(function () {
            var expr = input.value.trim();
            if (!expr) {
                updatePreview('Next run: —');
                return;
            }

            fetch('/api/schedules/preview', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/x-www-form-urlencoded; charset=UTF-8',
                    'X-CSRF-Token': getCsrfToken()
                },
                body: 'cron=' + encodeURIComponent(expr)
            })
                .then(function (resp) {
                    return resp.json().then(function (data) {
                        return { ok: resp.ok, data: data };
                    });
                })
                .then(function (result) {
                    if (!result.ok || !result.data || !result.data.ok) {
                        var msg = (result.data && result.data.message) ? result.data.message : 'Invalid cron';
                        updatePreview('Next run: ' + msg);
                        return;
                    }
                    if (!Array.isArray(result.data.next_runs) || result.data.next_runs.length === 0) {
                        updatePreview('Next run: none');
                        return;
                    }
                    updatePreview('Next runs: ' + result.data.next_runs.join(' | '));
                })
                .catch(function () {
                    updatePreview('Next run: unable to preview');
                });
        }, 250);
    });
})();
