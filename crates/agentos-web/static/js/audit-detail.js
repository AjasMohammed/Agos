// Copies the text content of the element identified by data-target to the clipboard.
function copyDetails(btn) {
    var targetId = btn.getAttribute('data-target');
    var pre = document.getElementById(targetId);
    if (!pre) return;
    navigator.clipboard.writeText(pre.textContent).then(function () {
        var orig = btn.textContent;
        btn.textContent = 'Copied!';
        setTimeout(function () { btn.textContent = orig; }, 1500);
    });
}
