/**
 * file-mention.js — @mention typeahead for file references in chat textareas.
 *
 * Attaches to textareas with [data-file-mention]. When the user types "@",
 * a dropdown appears with fuzzy-matched file suggestions from the server.
 * Selecting a file inserts `@filename` into the textarea.
 *
 * Usage: <textarea data-file-mention data-session-id="optional-uuid"></textarea>
 */
(function () {
  'use strict';

  const DEBOUNCE_MS = 200;
  const MIN_QUERY_LEN = 0; // Show suggestions immediately on @
  const MAX_RESULTS = 12;

  /** Simple debounce helper. */
  function debounce(fn, ms) {
    let timer;
    return function (...args) {
      clearTimeout(timer);
      timer = setTimeout(() => fn.apply(this, args), ms);
    };
  }

  /** MIME to emoji icon mapping for visual distinction. */
  function mimeIcon(mime) {
    if (!mime) return '📄';
    if (mime.startsWith('image/')) return '🖼️';
    if (mime.startsWith('video/')) return '🎬';
    if (mime.startsWith('audio/')) return '🎵';
    if (mime.includes('pdf')) return '📕';
    if (mime.includes('zip') || mime.includes('tar') || mime.includes('gzip')) return '📦';
    if (mime.includes('json') || mime.includes('xml') || mime.includes('yaml')) return '📋';
    if (mime.startsWith('text/')) return '📝';
    return '📄';
  }

  /** Format file size for display. */
  function formatSize(sizeKb) {
    if (!sizeKb || sizeKb < 1) return '<1 KB';
    if (sizeKb < 1024) return sizeKb + ' KB';
    return (sizeKb / 1024).toFixed(1) + ' MB';
  }

  /**
   * Create and manage a typeahead dropdown for a single textarea.
   */
  function createMentionTypeahead(textarea) {
    const sessionId = textarea.dataset.sessionId || '';
    let dropdown = null;
    let visible = false;
    let items = [];
    let selectedIndex = -1;
    let mentionStart = -1; // cursor position of the @ character
    let abortCtrl = null;

    function createDropdown() {
      const el = document.createElement('div');
      el.className = 'file-mention-dropdown';
      el.style.cssText = 'position:absolute;z-index:9999;display:none;';
      el.setAttribute('role', 'listbox');
      el.setAttribute('aria-label', 'File suggestions');
      el._mentionTextarea = textarea; // Back-reference for cleanup.
      document.body.appendChild(el);
      return el;
    }

    function positionDropdown() {
      if (!dropdown || !visible) return;
      // Use a hidden span to measure caret position in the textarea.
      const rect = textarea.getBoundingClientRect();
      const lineHeight = parseInt(getComputedStyle(textarea).lineHeight) || 20;
      // Position below the textarea input area.
      dropdown.style.left = rect.left + window.scrollX + 'px';
      dropdown.style.top = (rect.bottom + window.scrollY + 4) + 'px';
      dropdown.style.width = Math.min(rect.width, 400) + 'px';
    }

    function renderDropdown() {
      if (!dropdown) dropdown = createDropdown();
      if (items.length === 0) {
        dropdown.innerHTML = '<div class="file-mention-empty">No files found</div>';
      } else {
        dropdown.innerHTML = items.map(function (item, i) {
          var cls = 'file-mention-item' + (i === selectedIndex ? ' selected' : '');
          return '<div class="' + cls + '" data-index="' + i + '" role="option"' +
            (i === selectedIndex ? ' aria-selected="true"' : '') + '>' +
            '<span class="file-mention-icon">' + mimeIcon(item.mime) + '</span>' +
            '<span class="file-mention-name">' + escapeHtml(item.original_name || item.name) + '</span>' +
            '<span class="file-mention-meta">' + escapeHtml(formatSize(item.size_kb)) + '</span>' +
            '</div>';
        }).join('');
      }
      dropdown.style.display = 'block';
      visible = true;
      positionDropdown();

      // Attach click handlers.
      dropdown.querySelectorAll('.file-mention-item').forEach(function (el) {
        el.addEventListener('mousedown', function (e) {
          e.preventDefault(); // Prevent textarea blur.
          selectItem(parseInt(el.dataset.index, 10));
        });
      });
    }

    function hideDropdown() {
      if (dropdown) dropdown.style.display = 'none';
      visible = false;
      items = [];
      selectedIndex = -1;
      mentionStart = -1;
      if (abortCtrl) { abortCtrl.abort(); abortCtrl = null; }
    }

    function selectItem(index) {
      if (index < 0 || index >= items.length) return;
      var item = items[index];
      var name = item.name || item.original_name;
      // Replace the @query with @name
      var val = textarea.value;
      var before = val.substring(0, mentionStart);
      var after = val.substring(textarea.selectionStart);
      textarea.value = before + '@' + name + ' ' + after;
      // Position cursor after the inserted mention.
      var newPos = mentionStart + 1 + name.length + 1;
      textarea.setSelectionRange(newPos, newPos);
      textarea.focus();
      hideDropdown();
      // Trigger input event for Alpine.js reactivity.
      textarea.dispatchEvent(new Event('input', { bubbles: true }));
    }

    var fetchSuggestions = debounce(function (query) {
      if (abortCtrl) abortCtrl.abort();
      abortCtrl = new AbortController();

      var url = '/api/files/search?q=' + encodeURIComponent(query);
      if (sessionId) url += '&session_id=' + encodeURIComponent(sessionId);

      fetch(url, {
        signal: abortCtrl.signal,
        credentials: 'same-origin'
      })
        .then(function (r) { return r.ok ? r.json() : { files: [] }; })
        .then(function (data) {
          items = (data.files || []).slice(0, MAX_RESULTS);
          selectedIndex = items.length > 0 ? 0 : -1;
          renderDropdown();
        })
        .catch(function (e) {
          if (e.name !== 'AbortError') {
            items = [];
            selectedIndex = -1;
            renderDropdown();
          }
        });
    }, DEBOUNCE_MS);

    /** Detect @ trigger and extract query from current cursor position. */
    function detectMention() {
      var pos = textarea.selectionStart;
      var val = textarea.value;
      // Walk backwards from cursor to find the @ trigger.
      var i = pos - 1;
      while (i >= 0) {
        var ch = val[i];
        if (ch === '@') {
          // Found @. Check it's at start of input or preceded by whitespace.
          if (i === 0 || /\s/.test(val[i - 1])) {
            mentionStart = i;
            var query = val.substring(i + 1, pos);
            // Only trigger if query doesn't contain spaces (single word).
            if (!/\s/.test(query)) {
              return query;
            }
          }
          break;
        }
        if (/\s/.test(ch)) break; // Hit whitespace before finding @.
        i--;
      }
      return null;
    }

    // --- Event handlers ---

    textarea.addEventListener('input', function () {
      var query = detectMention();
      if (query !== null) {
        fetchSuggestions(query);
      } else {
        hideDropdown();
      }
    });

    textarea.addEventListener('keydown', function (e) {
      if (!visible) return;
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        selectedIndex = Math.min(selectedIndex + 1, items.length - 1);
        renderDropdown();
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        selectedIndex = Math.max(selectedIndex - 1, 0);
        renderDropdown();
      } else if (e.key === 'Enter' || e.key === 'Tab') {
        if (selectedIndex >= 0) {
          e.preventDefault();
          e.stopPropagation();
          selectItem(selectedIndex);
        }
      } else if (e.key === 'Escape') {
        e.preventDefault();
        hideDropdown();
      }
    });

    textarea.addEventListener('blur', function () {
      // Small delay to allow click events on dropdown items to fire first.
      setTimeout(function () { hideDropdown(); }, 150);
    });

    // Reposition on scroll/resize.
    window.addEventListener('scroll', positionDropdown, { passive: true });
    window.addEventListener('resize', positionDropdown, { passive: true });
  }

  function escapeHtml(s) {
    var d = document.createElement('div');
    d.textContent = s;
    return d.innerHTML;
  }

  /** Initialize on DOM ready and on dynamic content (htmx). */
  function initAll() {
    // Clean up dropdowns for textareas that were removed from the DOM (htmx swap).
    document.querySelectorAll('.file-mention-dropdown').forEach(function (el) {
      if (el._mentionTextarea && !document.body.contains(el._mentionTextarea)) {
        el.remove();
      }
    });
    document.querySelectorAll('textarea[data-file-mention]:not([data-mention-init])').forEach(function (el) {
      el.setAttribute('data-mention-init', '1');
      createMentionTypeahead(el);
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initAll);
  } else {
    initAll();
  }
  // Re-init after htmx swaps.
  document.addEventListener('htmx:afterSettle', initAll);
})();
