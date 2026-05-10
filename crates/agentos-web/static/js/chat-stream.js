(function () {
    function escapeHtml(s) {
        return String(s)
            .replace(/&/g, "&amp;")
            .replace(/</g, "&lt;")
            .replace(/>/g, "&gt;")
            .replace(/\"/g, "&quot;")
            .replace(/'/g, "&#39;");
    }

    // Format raw tool call data for human-readable display.
    // Strips XML wrapper tags, unwraps MCP content envelopes,
    // and recursively parses nested stringified JSON.
    function formatToolData(raw) {
        if (!raw || typeof raw !== "string") return raw || "";
        var s = raw.trim();

        // Strip common XML wrapper tags
        var tagPairs = [
            ["<user_data>", "</user_data>"],
            ["<tool_result>", "</tool_result>"],
            ["<tool_input>", "</tool_input>"],
            ["<result>", "</result>"]
        ];
        for (var i = 0; i < tagPairs.length; i++) {
            var open = tagPairs[i][0], close = tagPairs[i][1];
            if (s.indexOf(open) === 0 && s.lastIndexOf(close) === s.length - close.length) {
                s = s.slice(open.length, s.length - close.length).trim();
                break;
            }
        }

        // Try to parse as JSON
        var parsed;
        try { parsed = JSON.parse(s); } catch (_) { return s; }

        // Unwrap MCP content envelope
        if (parsed && parsed.content && Array.isArray(parsed.content)) {
            var texts = parsed.content
                .filter(function (c) { return c.type === "text" && c.text; })
                .map(function (c) { return c.text; });
            if (texts.length > 0) {
                var combined = texts.join("\n");
                try { parsed = JSON.parse(combined); } catch (_) { return combined; }
            }
        }

        // Deep-resolve stringified JSON values (up to 3 levels)
        function deepResolve(obj, depth) {
            if (depth > 3) return obj;
            if (typeof obj === "string") {
                var t = obj.trim();
                if ((t[0] === '{' && t[t.length - 1] === '}') || (t[0] === '[' && t[t.length - 1] === ']')) {
                    try { return deepResolve(JSON.parse(t), depth + 1); } catch (_) {}
                }
                return obj;
            }
            if (Array.isArray(obj)) {
                return obj.map(function (v) { return deepResolve(v, depth); });
            }
            if (obj && typeof obj === "object") {
                var out = {};
                for (var k in obj) {
                    if (Object.prototype.hasOwnProperty.call(obj, k)) {
                        out[k] = deepResolve(obj[k], depth);
                    }
                }
                return out;
            }
            return obj;
        }

        parsed = deepResolve(parsed, 0);

        if (typeof parsed === "string") return parsed;
        try { return JSON.stringify(parsed, null, 2); } catch (_) { return s; }
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
                    if (inOl) { out.push("</ol>"); inOl = false; }
                    out.push("<ul>"); inUl = true;
                }
                out.push("<li>" + line.replace(/^\s*[-*]\s+/, "") + "</li>");
                continue;
            }
            if (/^\s*\d+\.\s+/.test(line)) {
                if (!inOl) {
                    if (inUl) { out.push("</ul>"); inUl = false; }
                    out.push("<ol>"); inOl = true;
                }
                out.push("<li>" + line.replace(/^\s*\d+\.\s+/, "") + "</li>");
                continue;
            }
            if (inUl) { out.push("</ul>"); inUl = false; }
            if (inOl) { out.push("</ol>"); inOl = false; }
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
        var attrRaw = el.getAttribute("data-raw-markdown");
        if (attrRaw != null && attrRaw !== el.dataset.rawMarkdown) {
            // Attribute set server-side (stored messages) or updated — sync to dataset.
            el.dataset.rawMarkdown = attrRaw;
        } else if (el.dataset.rawMarkdown == null) {
            el.dataset.rawMarkdown = el.textContent || "";
        }
        el.innerHTML = renderMarkdown(el.dataset.rawMarkdown);
        highlightCode(el);
    }

    function renderAllMarkdown(root) {
        var scope = root || document;
        var nodes = [];
        if (scope.matches && scope.matches(".markdown-content")) {
            nodes.push(scope);
        }
        nodes = nodes.concat(Array.from(scope.querySelectorAll(".markdown-content")));
        for (var i = 0; i < nodes.length; i++) {
            renderMarkdownInto(nodes[i]);
        }
    }

    function parseEventData(data) {
        try { return JSON.parse(data); } catch (_) { return null; }
    }

    function attachStream(container) {
        if (!container || container.dataset.streamAttached === "1") return;
        container.dataset.streamAttached = "1";

        var sessionId = container.dataset.sessionId;
        if (!sessionId) return;

        var thinking   = container.querySelector('[data-role="chat-thinking-indicator"]');
        var responseDiv = container.querySelector('[data-role="chat-stream-response"]');
        var contentEl  = container.querySelector('[data-role="chat-stream-content"]');
        var metaEl = container.querySelector('[data-role="chat-stream-meta"]');
        var tokenEl = container.querySelector('[data-role="chat-stream-tokens"]');
        var costEl = container.querySelector('[data-role="chat-stream-cost"]');
        var stopBtn = container.querySelector('[data-role="chat-stream-stop"]');
        var copyBtn = container.querySelector('[data-role="chat-stream-copy"]');
        var msgList    = document.getElementById("chat-messages-list");

        // currentTextEl: the active text segment div. Nulled when a tool call starts
        // so the next text-delta creates a fresh segment after the tool card.
        var currentTextEl = null;
        var hasContent    = false;

        var renderQueued = false;
        var pendingHighlightRoot = null;
        var shouldFollowStream = true;

        function isNearBottom() {
            if (!msgList) return true;
            return msgList.scrollHeight - msgList.scrollTop - msgList.clientHeight < 96;
        }

        function scrollToBottom(force) {
            if (!msgList) return;
            if (force || shouldFollowStream || isNearBottom()) {
                msgList.scrollTop = msgList.scrollHeight;
            }
        }

        function queueRender(root) {
            if (root) pendingHighlightRoot = root;
            if (renderQueued) return;
            renderQueued = true;
            window.requestAnimationFrame(function () {
                renderQueued = false;
                if (currentTextEl) {
                    currentTextEl.innerHTML = renderMarkdown(currentTextEl.dataset.rawMarkdown || "");
                    pendingHighlightRoot = currentTextEl;
                }
                if (pendingHighlightRoot) {
                    highlightCode(pendingHighlightRoot);
                    pendingHighlightRoot = null;
                }
                scrollToBottom();
            });
        }

        function showResponseIfNeeded() {
            if (!hasContent) {
                hasContent = true;
                if (thinking) thinking.style.display = "none";
                if (responseDiv) responseDiv.style.display = "";
                if (metaEl) metaEl.style.display = "";
            }
        }

        function setThinking(iteration) {
            if (!thinking) return;
            thinking.style.display = "";
            var label = thinking.querySelector(".muted");
            if (label) {
                label.textContent = iteration
                    ? ("Thinking... (iteration " + iteration + ")")
                    : "Thinking...";
            }
        }

        // Create a new text segment div and append it to contentEl.
        function newTextSegment() {
            var el = document.createElement("div");
            el.className = "chat-text-segment markdown-content";
            el.dataset.rawMarkdown = "";
            if (contentEl) contentEl.appendChild(el);
            return el;
        }

        function appendToolCard(toolName, label, klass) {
            if (!contentEl) return null;
            var el = document.createElement("details");
            el.className = "chat-tool-details chat-tool-details-stream " + (klass || "");
            el.open = true;
            el.innerHTML =
                '<summary><small><strong></strong><span class="chat-tool-summary-status"></span></small></summary>' +
                '<pre class="chat-tool-result"><code class="chat-tool-stream-preview"></code></pre>';
            var titleEl = el.querySelector("strong");
            if (titleEl) titleEl.textContent = toolName || "tool";
            var statusEl = el.querySelector(".chat-tool-summary-status");
            if (statusEl) statusEl.textContent = label ? " · " + label : "";
            contentEl.appendChild(el);
            return el;
        }

        function updateToolCard(card, label, preview) {
            if (!card) return;
            var statusEl = card.querySelector(".chat-tool-summary-status");
            if (statusEl) statusEl.textContent = label ? " · " + label : "";
            var previewEl = card.querySelector(".chat-tool-stream-preview");
            if (previewEl) previewEl.textContent = formatToolData(preview || "");
        }

        function updateMeta(tokensUsed, costUsd) {
            if (tokenEl) tokenEl.textContent = tokensUsed ? ("· " + tokensUsed + " tokens") : "";
            if (costEl) costEl.textContent = costUsd ? ("· $" + Number(costUsd).toFixed(6)) : "";
        }

        function updateSessionTotals(tokensUsed, costUsd) {
            var totalTokensEl = document.getElementById("chat-total-tokens");
            if (totalTokensEl && tokensUsed) {
                var currentTokens = parseInt(totalTokensEl.textContent || "0", 10);
                if (!isNaN(currentTokens)) totalTokensEl.textContent = String(currentTokens + tokensUsed);
            }

            var totalToolsEl = document.getElementById("chat-total-tools");
            if (totalToolsEl) {
                var currentTools = parseInt(totalToolsEl.textContent || "0", 10);
                var streamTools = contentEl ? contentEl.querySelectorAll(".chat-tool-details-stream").length : 0;
                if (!isNaN(currentTools)) totalToolsEl.textContent = String(currentTools + streamTools);
            }

            var totalCostEl = document.getElementById("chat-total-cost");
            if (totalCostEl && costUsd) {
                var raw = (totalCostEl.textContent || "").replace("$", "").trim();
                var currentCost = raw && raw !== "--" ? Number(raw) : 0;
                if (!isNaN(currentCost)) totalCostEl.textContent = "$" + (currentCost + Number(costUsd)).toFixed(6);
            }
        }

        var pendingToolCards = {}; // tool_name → most recently appended running card

        function setStopDisabled(label) {
            if (!stopBtn) return;
            stopBtn.disabled = true;
            if (label) stopBtn.textContent = label;
        }

        if (stopBtn) {
            stopBtn.addEventListener("click", function () {
                if (stopBtn.disabled) return;
                var url = stopBtn.dataset.stopUrl;
                if (!url) return;
                stopBtn.disabled = true;
                stopBtn.textContent = "Stopping...";
                var meta = document.querySelector('meta[name="csrf-token"]');
                fetch(url, {
                    method: "POST",
                    credentials: "same-origin",
                    headers: meta && meta.content ? { "X-CSRF-Token": meta.content } : {}
                }).catch(function () {
                    stopBtn.disabled = false;
                    stopBtn.textContent = "Stop";
                });
            });
        }

        function textSegments() {
            return contentEl ? Array.from(contentEl.querySelectorAll(".chat-text-segment")) : [];
        }

        function accumulatedText() {
            return textSegments()
                .map(function (el) { return el.dataset.rawMarkdown || el.innerText || ""; })
                .join("")
                .trim();
        }

        function renderFinalAnswer(answer) {
            if (!contentEl || !answer) return;

            var existing = accumulatedText();
            if (existing === answer.trim()) {
                textSegments().forEach(function (el) {
                    el.innerHTML = renderMarkdown(el.dataset.rawMarkdown || "");
                    highlightCode(el);
                });
                scrollToBottom(true);
                return;
            }

            textSegments().forEach(function (el) { el.remove(); });
            currentTextEl = newTextSegment();
            currentTextEl.dataset.rawMarkdown = answer;
            currentTextEl.innerHTML = renderMarkdown(answer);
            highlightCode(currentTextEl);
            scrollToBottom(true);
        }

        var es = new EventSource("/chat/" + encodeURIComponent(sessionId) + "/stream");

        es.addEventListener("chat-stream", function (e) {
            var d = parseEventData(e.data);
            if (!d || !d.type) return;

            shouldFollowStream = isNearBottom();

            switch (d.type) {
                case "thinking":
                    setThinking(d.iteration);
                    break;

                case "text-delta":
                    showResponseIfNeeded();
                    // Re-use the current text segment or start a new one.
                    if (!currentTextEl) {
                        currentTextEl = newTextSegment();
                    }
                    currentTextEl.dataset.rawMarkdown =
                        (currentTextEl.dataset.rawMarkdown || "") + (d.text || "");
                    currentTextEl.textContent = currentTextEl.dataset.rawMarkdown;
                    queueRender(currentTextEl);
                    break;

                case "tool-start": {
                    showResponseIfNeeded();
                    // Freeze the current text segment so the next text-delta starts fresh.
                    currentTextEl = null;
                    var toolName = d.tool_name || "tool";
                    var card = appendToolCard(toolName, "running", "chat-activity-running");
                    // Track so tool-result can find it.
                    if (!pendingToolCards[toolName]) pendingToolCards[toolName] = [];
                    pendingToolCards[toolName].push(card);
                    break;
                }

                case "tool-result": {
                    var toolName2 = d.tool_name || "tool";
                    var queue = pendingToolCards[toolName2];
                    var card2 = queue && queue.length ? queue.shift() : null;
                    if (card2) {
                        card2.classList.remove("chat-activity-running");
                        card2.classList.add(d.success ? "chat-activity-done" : "chat-activity-error");
                        updateToolCard(
                            card2,
                            (d.success ? "done" : "error") + " · " + (d.duration_ms || 0) + "ms",
                            d.result_preview || ""
                        );
                    }
                    // Freeze text segment so any following text starts in a new segment.
                    currentTextEl = null;
                    break;
                }

                case "done":
                    if (!hasContent && d.answer) {
                        showResponseIfNeeded();
                    }
                    renderFinalAnswer(d.answer || "");
                    updateMeta(d.tokens_used || 0, d.cost_usd || 0);
                    updateSessionTotals(d.tokens_used || 0, d.cost_usd || 0);
                    setStopDisabled(d.iterations === 0 ? "Stopped" : "Done");
                    es.close();
                    finalize();
                    break;

                case "error": {
                    showResponseIfNeeded();
                    var errEl = newTextSegment();
                    errEl.innerHTML =
                        '<p style="color:var(--pico-color-red-500)">' +
                        escapeHtml("Error: " + (d.message || "Unknown error")) + "</p>";
                    currentTextEl = null;
                    setStopDisabled("Stopped");
                    es.close();
                    finalize();
                    break;
                }

                default:
                    break;
            }

            scrollToBottom();
        });

        function finalize() {
            container.dataset.streamFinalized = "1";
            container.removeAttribute("data-role");
            if (thinking) thinking.remove();
            if (contentEl) contentEl.classList.remove("chat-streaming");
            setStopDisabled();
            if (copyBtn) {
                copyBtn.onclick = function () {
                    var text = Array.from(container.querySelectorAll(".chat-text-segment"))
                        .map(function (el) { return el.innerText; })
                        .join("\n\n")
                        .trim();
                    navigator.clipboard.writeText(text);
                };
            }
        }

        es.onerror = function () {
            es.close();
            finalize();
        };

        if (msgList && msgList.dataset.followListenerAttached !== "1") {
            msgList.dataset.followListenerAttached = "1";
            msgList.addEventListener("scroll", function () {
                shouldFollowStream = isNearBottom();
            }, { passive: true });
        }
    }

    function attachAllStreams(root) {
        var scope = root || document;
        var targets = [];
        if (scope.matches && scope.matches('[data-role="chat-stream-target"]:not([data-stream-attached="1"])')) {
            targets.push(scope);
        }
        targets = targets.concat(Array.from(scope.querySelectorAll('[data-role="chat-stream-target"]:not([data-stream-attached="1"])')));
        for (var i = 0; i < targets.length; i++) {
            attachStream(targets[i]);
        }
    }

    function observeDynamicChat() {
        var list = document.getElementById("chat-messages-list");
        if (!list || typeof MutationObserver === "undefined" || list.dataset.streamObserverAttached === "1") {
            return;
        }
        list.dataset.streamObserverAttached = "1";
        new MutationObserver(function (mutations) {
            mutations.forEach(function (mutation) {
                mutation.addedNodes.forEach(function (node) {
                    if (!node || node.nodeType !== 1) return;
                    renderAllMarkdown(node);
                    attachAllStreams(node);
                });
            });
        }).observe(list, { childList: true, subtree: true });
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
        observeDynamicChat();
    });

    document.body.addEventListener("htmx:afterSwap", function (event) {
        var target = event && event.detail && event.detail.target ? event.detail.target : document;
        renderAllMarkdown(target);
        attachAllStreams(target);
        observeDynamicChat();
    });

    document.body.addEventListener("htmx:afterSettle", function (event) {
        var target = event && event.detail && event.detail.target ? event.detail.target : document;
        renderAllMarkdown(target);
        attachAllStreams(target);
        observeDynamicChat();
    });
}());
