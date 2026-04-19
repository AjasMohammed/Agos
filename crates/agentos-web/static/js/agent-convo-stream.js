(function () {
    "use strict";

    function escapeHtml(s) {
        return String(s || "")
            .replace(/&/g, "&amp;")
            .replace(/</g, "&lt;")
            .replace(/>/g, "&gt;")
            .replace(/"/g, "&quot;")
            .replace(/'/g, "&#39;");
    }

    function renderMarkdown(raw) {
        if (window.marked && typeof window.marked.parse === "function"
            && window.DOMPurify && typeof window.DOMPurify.sanitize === "function") {
            try {
                var html = window.marked.parse(raw || "", { breaks: true, gfm: true });
                return window.DOMPurify.sanitize(html);
            } catch (_) { /* fall through */ }
        }
        // Minimal fallback renderer.
        var text = String(raw || "")
            .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
        var codeBlocks = [];
        text = text.replace(/```([a-zA-Z0-9_-]+)?\r?\n([\s\S]*?)```/g, function (_, lang, code) {
            var cls = lang ? ' class="language-' + lang + '"' : "";
            var token = "@@CODE_" + codeBlocks.length + "@@";
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
        var out = [], inUl = false, inOl = false;
        for (var i = 0; i < lines.length; i++) {
            var line = lines[i];
            if (/^\s*[-*]\s+/.test(line)) {
                if (!inUl) { if (inOl) { out.push("</ol>"); inOl = false; } out.push("<ul>"); inUl = true; }
                out.push("<li>" + line.replace(/^\s*[-*]\s+/, "") + "</li>"); continue;
            }
            if (/^\s*\d+\.\s+/.test(line)) {
                if (!inOl) { if (inUl) { out.push("</ul>"); inUl = false; } out.push("<ol>"); inOl = true; }
                out.push("<li>" + line.replace(/^\s*\d+\.\s+/, "") + "</li>"); continue;
            }
            if (inUl) { out.push("</ul>"); inUl = false; }
            if (inOl) { out.push("</ol>"); inOl = false; }
            if (line.trim() === "") { out.push(""); }
            else if (!/^<h[1-3]>/.test(line) && !/^<pre>/.test(line)) { out.push("<p>" + line + "</p>"); }
            else { out.push(line); }
        }
        if (inUl) out.push("</ul>");
        if (inOl) out.push("</ol>");
        var html = out.join("\n");
        for (var c = 0; c < codeBlocks.length; c++) {
            html = html.replace("@@CODE_" + c + "@@", codeBlocks[c]);
        }
        return html;
    }

    // ── State ────────────────────────────────────────────────────────────────

    var es = null;
    var convoId = null;
    var timeline = null;
    var statusBar = null;
    var stopBtn = null;
    var participantColors = {};

    // Per-turn state: key = "turn:agent"
    // { row, bubble, thinkingEl, currentTextEl, pendingToolCards: {toolName: [card,...]} }
    var turnStates = {};

    // ── DOM helpers ──────────────────────────────────────────────────────────

    function getColor(agent) { return participantColors[agent] || "#888"; }
    function getInitial(agent) { return (agent || "?").charAt(0).toUpperCase(); }

    function setStatus(text, cls) {
        if (!statusBar) return;
        statusBar.textContent = text;
        statusBar.className = "convo-status-bar " + (cls || "");
    }

    function scrollToBottom() {
        if (timeline) timeline.scrollTop = timeline.scrollHeight;
    }

    // ── Turn bubble creation ─────────────────────────────────────────────────

    function ensureTurn(agent, turn) {
        var key = turn + ":" + agent;
        if (turnStates[key]) return turnStates[key];

        var color = getColor(agent);
        var initial = getInitial(agent);

        var row = document.createElement("div");
        row.className = "convo-turn-row";
        row.setAttribute("data-turn", turn);
        row.setAttribute("data-agent", agent);

        var avatar = document.createElement("div");
        avatar.className = "convo-avatar";
        avatar.textContent = initial;
        avatar.style.background = color;

        var col = document.createElement("div");
        col.className = "convo-agent-col";

        var header = document.createElement("div");
        header.className = "convo-agent-header";
        header.innerHTML =
            '<span class="convo-agent-name" style="color:' + color + '">' + escapeHtml(agent) + "</span>" +
            '<span class="convo-turn-label muted">Turn ' + turn + "</span>";

        var thinkingEl = document.createElement("div");
        thinkingEl.className = "convo-thinking";
        thinkingEl.innerHTML =
            '<div class="chat-thinking-dots"><span></span><span></span><span></span></div>' +
            '<span class="muted">Thinking…</span>';

        // The bubble is the single interleaved container for tool cards + text segments.
        var bubble = document.createElement("div");
        bubble.className = "convo-bubble convo-bubble-content chat-streaming";

        col.appendChild(header);
        col.appendChild(thinkingEl);
        col.appendChild(bubble);
        row.appendChild(avatar);
        row.appendChild(col);

        if (timeline) timeline.appendChild(row);
        scrollToBottom();

        var state = {
            row: row,
            bubble: bubble,
            thinkingEl: thinkingEl,
            currentTextEl: null,        // active text segment inside bubble
            pendingToolCards: {},       // toolName → [card, ...]
        };
        turnStates[key] = state;
        return state;
    }

    function hideThinking(state) {
        if (state && state.thinkingEl) state.thinkingEl.style.display = "none";
    }

    // Append a new text-segment div to the bubble and return it.
    function newTextSegment(state) {
        var el = document.createElement("div");
        el.className = "chat-text-segment markdown-content";
        el.dataset.rawMarkdown = "";
        state.bubble.appendChild(el);
        state.currentTextEl = el;
        return el;
    }

    // Append a tool pill to the bubble and return it.
    function appendToolCard(state, toolName) {
        var card = document.createElement("div");
        card.className = "chat-activity chat-activity-tool chat-activity-running";
        card.setAttribute("data-tool", toolName);
        card.innerHTML =
            '<span class="chat-activity-icon">⚙</span>' +
            '<span class="chat-activity-label">Using ' + escapeHtml(toolName) + "…</span>";
        state.bubble.appendChild(card);
        if (!state.pendingToolCards[toolName]) state.pendingToolCards[toolName] = [];
        state.pendingToolCards[toolName].push(card);
        // Freeze current text segment so next text-delta starts a new one after the card.
        state.currentTextEl = null;
        return card;
    }

    // ── Event handlers ────────────────────────────────────────────────────────

    function onTurnStart(data) {
        setStatus(data.agent + " is thinking… (Turn " + data.turn + ")", "running");
        ensureTurn(data.agent, data.turn);
    }

    function onThinking(data) {
        var s = ensureTurn(data.agent, data.turn);
        if (s.thinkingEl) s.thinkingEl.style.display = "";
    }

    function onTextChunk(data) {
        if (!data.agent) return;
        var s = ensureTurn(data.agent, data.turn);
        hideThinking(s);
        if (!s.currentTextEl) newTextSegment(s);
        s.currentTextEl.dataset.rawMarkdown =
            (s.currentTextEl.dataset.rawMarkdown || "") + (data.text || "");
        s.currentTextEl.innerHTML = renderMarkdown(s.currentTextEl.dataset.rawMarkdown);
        s.bubble.style.display = "";
        scrollToBottom();
    }

    function onToolStart(data) {
        var s = ensureTurn(data.agent, data.turn);
        hideThinking(s);
        appendToolCard(s, data.tool_name || "tool");
        scrollToBottom();
    }

    function onToolResult(data) {
        var s = ensureTurn(data.agent, data.turn);
        var toolName = data.tool_name || "tool";
        var queue = s.pendingToolCards[toolName];
        var card = queue && queue.length ? queue.shift() : null;
        if (!card) return;

        card.classList.remove("chat-activity-running");
        card.classList.add(data.success ? "chat-activity-done" : "chat-activity-error");

        var labelEl = card.querySelector(".chat-activity-label");
        if (labelEl) {
            var suffix = (data.duration_ms || 0) + "ms";
            if (data.result_preview) suffix += " · " + data.result_preview;
            labelEl.textContent = toolName + " (" + suffix + ")";
        }
        // Freeze text segment so any following text starts fresh after this card.
        s.currentTextEl = null;
    }

    function onTurnEnd(data) {
        var s = ensureTurn(data.agent, data.turn);
        hideThinking(s);
        // If the final answer is longer than what we accumulated, render it in a new segment.
        if (data.answer) {
            var accumulated = Array.from(s.bubble.querySelectorAll(".chat-text-segment"))
                .map(function (el) { return el.dataset.rawMarkdown || ""; })
                .join("");
            if (!accumulated || accumulated.length < data.answer.length) {
                // Clear existing text segments and replace with the complete answer.
                Array.from(s.bubble.querySelectorAll(".chat-text-segment")).forEach(function (el) {
                    el.remove();
                });
                s.currentTextEl = null;
                var seg = newTextSegment(s);
                seg.dataset.rawMarkdown = data.answer;
                seg.innerHTML = renderMarkdown(data.answer);
            }
        }
        s.bubble.classList.remove("chat-streaming");
        s.bubble.style.display = "";
        setStatus(data.agent + " finished Turn " + data.turn, "running");
        scrollToBottom();
    }

    function onConversationDone(data) {
        convoDone = true;
        if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; }
        setStatus("Conversation complete — " + data.total_turns + " turns", "complete");
        if (stopBtn) stopBtn.style.display = "none";
        if (es) { es.close(); es = null; }
        var badge = document.getElementById("convo-status-badge");
        if (badge) {
            badge.textContent = "complete";
            badge.className = "status-badge status-complete";
        }
    }

    function onError(data) {
        setStatus("Error: " + (data.message || "unknown"), "error");
        var row = document.createElement("div");
        row.className = "convo-error-row";
        row.innerHTML = '<span class="error-icon">⚠</span> ' + escapeHtml(data.message || "Unknown error");
        if (timeline) timeline.appendChild(row);
        scrollToBottom();
    }

    // ── SSE connection ────────────────────────────────────────────────────────

    var convoDone = false;
    var retries = 0;
    var retryTimer = null;
    var MAX_RETRIES = 6;

    function connect() {
        if (convoDone || !convoId) return;
        if (es) { es.close(); es = null; }
        es = new EventSource("/agent-chat/" + convoId + "/stream");

        es.addEventListener("convo-stream", function (e) {
            var data;
            try { data = JSON.parse(e.data); } catch (_) { return; }

            switch (data.type) {
                case "TurnStart":        onTurnStart(data); break;
                case "Thinking":         onThinking(data); break;
                case "TextChunk":        onTextChunk(data); break;
                case "ToolStart":        onToolStart(data); break;
                case "ToolResult":       onToolResult(data); break;
                case "TurnEnd":          onTurnEnd(data); break;
                case "ConversationDone": onConversationDone(data); break;
                case "Error":            onError(data); break;
            }
        });

        es.onopen = function () { retries = 0; };

        es.onerror = function () {
            if (convoDone) return;
            if (es) { es.close(); es = null; }
            if (retries < MAX_RETRIES) {
                var delay = Math.min(500 * Math.pow(2, retries), 15000);
                retries++;
                setStatus("Reconnecting… (attempt " + retries + ")", "running");
                retryTimer = setTimeout(function () { retryTimer = null; connect(); }, delay);
            } else {
                setStatus("Connection lost — reload to reconnect", "error");
            }
        };
    }

    // ── Init ──────────────────────────────────────────────────────────────────

    function init() {
        var el = document.getElementById("convo-stream-root");
        if (!el) return;

        convoId = el.getAttribute("data-convo-id");
        var isActive = el.getAttribute("data-is-active") === "true";
        timeline = document.getElementById("convo-timeline");
        statusBar = document.getElementById("convo-status-bar");
        stopBtn = document.getElementById("convo-stop-btn");

        document.querySelectorAll("[data-participant-color]").forEach(function (node) {
            var name = node.getAttribute("data-participant-name");
            var color = node.getAttribute("data-participant-color");
            if (name && color) participantColors[name] = color;
        });

        if (isActive) {
            setStatus("Connecting…", "running");
            connect();
        }
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", init);
    } else {
        init();
    }
})();
