(function () {
    function escapeHtml(s) {
        return String(s)
            .replace(/&/g, "&amp;")
            .replace(/</g, "&lt;")
            .replace(/>/g, "&gt;")
            .replace(/\"/g, "&quot;")
            .replace(/'/g, "&#39;");
    }

    function renderMarkdownLite(raw) {
        var text = escapeHtml(raw || "");
        var codeBlocks = [];
        text = text.replace(/```([a-zA-Z0-9_-]+)?\r?\n([\s\S]*?)```/g, function (_, lang, code) {
            var cls = lang ? ' class="language-' + lang + '"' : "";
            var token = "@@CODE_BLOCK_" + codeBlocks.length + "@@";
            codeBlocks.push("<pre><code" + cls + ">" + code + "</code></pre>");
            return token;
        });
        text = text.replace(/`([^`\n]+)`/g, "<code>$1</code>");
        text = text.replace(/^### (.*)$/gm, "<h3>$1</h3>");
        text = text.replace(/^## (.*)$/gm, "<h2>$1</h2>");
        text = text.replace(/^# (.*)$/gm, "<h1>$1</h1>");
        text = text.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
        text = text.replace(/\*([^*\n]+)\*/g, "<em>$1</em>");

        var lines = text.split("\n");
        var out = [];
        var inUl = false;
        var inOl = false;
        for (var i = 0; i < lines.length; i++) {
            var line = lines[i];
            if (/^\s*[-*]\s+/.test(line)) {
                if (!inUl) {
                    if (inOl) {
                        out.push("</ol>");
                        inOl = false;
                    }
                    out.push("<ul>");
                    inUl = true;
                }
                out.push("<li>" + line.replace(/^\s*[-*]\s+/, "") + "</li>");
                continue;
            }
            if (/^\s*\d+\.\s+/.test(line)) {
                if (!inOl) {
                    if (inUl) {
                        out.push("</ul>");
                        inUl = false;
                    }
                    out.push("<ol>");
                    inOl = true;
                }
                out.push("<li>" + line.replace(/^\s*\d+\.\s+/, "") + "</li>");
                continue;
            }
            if (inUl) {
                out.push("</ul>");
                inUl = false;
            }
            if (inOl) {
                out.push("</ol>");
                inOl = false;
            }
            if (line.trim() === "") {
                out.push("");
            } else if (!/^<h[1-3]>/.test(line) && !/^<pre>/.test(line)) {
                out.push("<p>" + line + "</p>");
            } else {
                out.push(line);
            }
        }
        if (inUl) out.push("</ul>");
        if (inOl) out.push("</ol>");

        var html = out.join("\n");
        for (var c = 0; c < codeBlocks.length; c++) {
            html = html.replace("@@CODE_BLOCK_" + c + "@@", codeBlocks[c]);
        }
        return html;
    }

    function renderMarkdown(raw) {
        if (window.marked && typeof window.marked.parse === "function"
            && window.DOMPurify && typeof window.DOMPurify.sanitize === "function") {
            try {
                var html = window.marked.parse(raw || "", { breaks: true, gfm: true });
                return window.DOMPurify.sanitize(html);
            } catch (_) {
                return renderMarkdownLite(raw);
            }
        }
        return renderMarkdownLite(raw);
    }

    function highlightCode(root) {
        if (!root || !window.hljs || typeof window.hljs.highlightElement !== "function") return;
        var blocks = root.querySelectorAll("pre code");
        for (var i = 0; i < blocks.length; i++) {
            window.hljs.highlightElement(blocks[i]);
        }
    }

    function renderMarkdownInto(el) {
        if (!el) return;
        if (el.dataset.rawMarkdown == null) {
            el.dataset.rawMarkdown = el.textContent || "";
        }
        el.innerHTML = renderMarkdown(el.dataset.rawMarkdown);
        highlightCode(el);
    }

    function renderAllMarkdown(root) {
        var scope = root || document;
        var nodes = scope.querySelectorAll(".markdown-content");
        for (var i = 0; i < nodes.length; i++) {
            renderMarkdownInto(nodes[i]);
        }
    }

    function parseEventData(data) {
        try {
            return JSON.parse(data);
        } catch (_) {
            return null;
        }
    }

    function attachStream(container) {
        if (!container || container.dataset.streamAttached === "1") return;
        container.dataset.streamAttached = "1";

        var sessionId = container.dataset.sessionId;
        if (!sessionId) return;

        var thinking = container.querySelector('[data-role="chat-thinking-indicator"]');
        var responseDiv = container.querySelector('[data-role="chat-stream-response"]');
        var textDiv = container.querySelector('[data-role="chat-stream-text"]');
        var activityList = container.querySelector('[data-role="chat-activity-list"]');
        var msgList = document.getElementById("chat-messages-list");

        var hasText = false;
        var rawMarkdown = "";

        function scrollToBottom() {
            if (msgList) msgList.scrollTop = msgList.scrollHeight;
        }

        function showResponseIfNeeded() {
            if (!hasText) {
                hasText = true;
                if (thinking) thinking.style.display = "none";
                if (responseDiv) responseDiv.style.display = "";
            }
        }

        function setThinking(iteration) {
            if (!thinking) return;
            thinking.style.display = "";
            var label = thinking.querySelector(".muted");
            if (label) {
                label.textContent = iteration ? ("Thinking... (iteration " + iteration + ")") : "Thinking...";
            }
        }

        function appendActivity(label, klass) {
            if (!activityList) return;
            var el = document.createElement("div");
            el.className = "chat-activity " + (klass || "");
            el.innerHTML = '<span class="chat-activity-icon">•</span><span class="chat-activity-label"></span>';
            var target = el.querySelector(".chat-activity-label");
            if (target) target.textContent = label;
            activityList.appendChild(el);
        }

        function renderResponse() {
            if (!textDiv) return;
            textDiv.dataset.rawMarkdown = rawMarkdown;
            textDiv.innerHTML = renderMarkdown(rawMarkdown);
            highlightCode(textDiv);
        }

        function finalize() {
            container.dataset.streamFinalized = "1";
            container.removeAttribute("data-role");
            if (thinking) thinking.remove();
            if (textDiv) textDiv.classList.remove("chat-streaming");
        }

        var es = new EventSource("/chat/" + encodeURIComponent(sessionId) + "/stream");

        es.addEventListener("chat-stream", function (e) {
            var d = parseEventData(e.data);
            if (!d || !d.type) return;

            switch (d.type) {
                case "thinking":
                    setThinking(d.iteration);
                    break;
                case "text-delta":
                    showResponseIfNeeded();
                    rawMarkdown += d.text || "";
                    renderResponse();
                    break;
                case "tool-start":
                    appendActivity("Using " + (d.tool_name || "tool") + "...", "chat-activity-tool");
                    break;
                case "tool-result": {
                    var suffix = (d.duration_ms || 0) + "ms";
                    if (d.result_preview) suffix += " · " + d.result_preview;
                    appendActivity(
                        (d.tool_name || "tool") + " (" + suffix + ")",
                        d.success ? "chat-activity-done" : "chat-activity-error"
                    );
                    break;
                }
                case "done":
                    if (!hasText && d.answer) {
                        showResponseIfNeeded();
                        rawMarkdown = d.answer;
                        renderResponse();
                    }
                    es.close();
                    finalize();
                    break;
                case "error": {
                    showResponseIfNeeded();
                    rawMarkdown = "Error: " + (d.message || "Unknown error");
                    if (textDiv) {
                        textDiv.innerHTML = '<p style="color:var(--pico-color-red-500)">' + escapeHtml(rawMarkdown) + "</p>";
                    }
                    appendActivity("Stream error", "chat-activity-error");
                    es.close();
                    finalize();
                    break;
                }
                default:
                    break;
            }

            scrollToBottom();
        });

        es.onerror = function () {
            es.close();
            finalize();
        };
    }

    function attachAllStreams(root) {
        var scope = root || document;
        var targets = scope.querySelectorAll('[data-role="chat-stream-target"]:not([data-stream-attached="1"])');
        for (var i = 0; i < targets.length; i++) {
            attachStream(targets[i]);
        }
    }

    function onReady(fn) {
        if (document.readyState === "loading") {
            document.addEventListener("DOMContentLoaded", fn, { once: true });
        } else {
            fn();
        }
    }

    onReady(function () {
        renderAllMarkdown(document);
        attachAllStreams(document);
    });

    document.body.addEventListener("htmx:afterSwap", function (event) {
        var target = event && event.detail && event.detail.target ? event.detail.target : document;
        renderAllMarkdown(target);
        attachAllStreams(target);
    });
}());
