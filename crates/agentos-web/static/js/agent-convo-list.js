// Agent conversation list page - keep selected participants in sync.
(function () {
    var hidden = document.getElementById('participants-hidden');
    var button = document.getElementById('start-convo-btn');
    if (!hidden || !button) return;

    var checks = document.querySelectorAll('input[name="participant_check"]');
    if (!checks.length) return;

    function update() {
        var selected = [];
        checks.forEach(function (check) {
            if (check.checked) selected.push(check.value);
        });
        hidden.value = selected.join(',');
        button.disabled = selected.length < 2;
    }

    checks.forEach(function (check) {
        check.addEventListener('change', update);
    });
    update();
}());
