(function () {
    function autosizeTextarea(ta) {
        if (!ta) return;
        ta.style.height = "auto";
        ta.style.height = Math.min(ta.scrollHeight, 320) + "px";
    }

    // ── Reply form clearing after successful send ────────────
    document.body.addEventListener("htmx:afterRequest", function (e) {
        if (!e || !e.detail || !e.detail.elt || e.detail.elt.id !== "chat-reply-form") {
            return;
        }
        var xhr = e.detail.xhr;
        if (!xhr || xhr.status < 200 || xhr.status >= 300) {
            return;
        }
        var form = e.detail.elt;
        form.reset();
        var ta = form.querySelector('textarea[name="message"]');
        if (ta) {
            autosizeTextarea(ta);
            ta.focus();
        }
    });

    // ── Copy button on code blocks ───────────────────────────
    // Observe the chat messages area for new <pre> elements and inject copy buttons.
    function addCopyButtons(root) {
        var pres = (root || document).querySelectorAll(".markdown-content pre");
        pres.forEach(function (pre) {
            if (pre.querySelector(".code-copy-btn")) return;
            var btn = document.createElement("button");
            btn.className = "code-copy-btn";
            btn.textContent = "Copy";
            btn.setAttribute("type", "button");
            btn.addEventListener("click", function () {
                var code = pre.querySelector("code");
                var text = code ? code.innerText : pre.innerText;
                navigator.clipboard.writeText(text).then(function () {
                    btn.textContent = "Copied!";
                    setTimeout(function () { btn.textContent = "Copy"; }, 1500);
                });
            });
            pre.appendChild(btn);
        });
    }

    // Run on load and observe for dynamic content.
    addCopyButtons();
    var chatArea = document.getElementById("chat-messages-list");
    if (chatArea && typeof MutationObserver !== "undefined") {
        new MutationObserver(function () { addCopyButtons(chatArea); })
            .observe(chatArea, { childList: true, subtree: true });
    }

    // ── Auto-scroll chat to bottom on load ───────────────────
    if (chatArea) {
        chatArea.scrollTop = chatArea.scrollHeight;
    }

    document.querySelectorAll("textarea[data-autoresize]").forEach(function (ta) {
        autosizeTextarea(ta);
        ta.addEventListener("input", function () {
            autosizeTextarea(ta);
        });
    });
}());
