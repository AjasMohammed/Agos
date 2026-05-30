#!/usr/bin/env python3
"""
Build a multi-page, interactive HTML version of the AgentOS User Handbook.

Reads every chapter in docs/handbook/*.md, converts the Markdown to HTML
(headings, tables, fenced code with Pygments highlighting, Obsidian callouts,
lists, blockquotes, wikilinks, mermaid diagrams) and emits a small static
website under docs/handbook/site/:

    site/
      index.html            landing page (hero + chapter cards + overview)
      ch-01.html .. ch-28.html
      assets/handbook.css
      assets/handbook.js
      assets/search-index.js

Every page shares a sidebar (all chapters), an "On this page" table of
contents, cross-page client-side search, prev/next navigation, and a
light/dark theme toggle. No server is required — open index.html directly.
Only the 2 mermaid diagrams need network (CDN) and degrade to source text.
"""

import html
import json
import os
import re
import shutil
from pygments import highlight
from pygments.lexers import get_lexer_by_name
from pygments.formatters import HtmlFormatter
from pygments.util import ClassNotFound

HANDBOOK_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "handbook")
SITE_DIR = os.path.join(HANDBOOK_DIR, "site")
ASSET_DIR = os.path.join(SITE_DIR, "assets")
INDEX_FILE = "AgentOS Handbook Index.md"


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

def slugify(text):
    text = re.sub(r"<[^>]+>", "", text)
    text = text.strip().lower()
    text = re.sub(r"[^\w\s-]", "", text)
    text = re.sub(r"[\s_]+", "-", text)
    return text.strip("-") or "section"


def chapter_id(stem):
    m = re.match(r"^(\d+)", stem)
    if m:
        return f"ch-{m.group(1)}"
    return "ch-" + slugify(stem)


def chapter_title(stem):
    return re.sub(r"^\d+\s*[-–]\s*", "", stem).strip()


def chapter_num(stem):
    m = re.match(r"^(\d+)", stem)
    return m.group(1) if m else ""


def page_filename(cid):
    return "index.html" if cid == "ch-00" else f"{cid}.html"


# ---- global resolution maps (filled in prepass) ----
WIKI_MAP = {}          # file stem            -> cid
TITLE_MAP = {}         # lowercased title     -> cid
CHAPTER_HEADINGS = {}  # cid                  -> set(slug)
HEADING_PAGE = {}      # slug                 -> cid (first chapter defining it)
CURRENT_CID = "ch-00"  # set before parsing each chapter (for [[#anchor]] links)


def resolve_wiki_target(inner):
    inner = inner.strip()
    if inner in WIKI_MAP:
        return WIKI_MAP[inner]
    low = chapter_title(inner).lower()
    if low in TITLE_MAP:
        return TITLE_MAP[low]
    for title, cid in TITLE_MAP.items():
        if title.startswith(low) or low.startswith(title):
            return cid
    return None


# ---------------------------------------------------------------------------
# inline markdown -> html
# ---------------------------------------------------------------------------

_CODE_TOKENS = []


def _protect_inline_code(text):
    def repl(m):
        _CODE_TOKENS.append(html.escape(m.group(1)))
        return f"\x00CODE{len(_CODE_TOKENS) - 1}\x00"
    return re.sub(r"`([^`]+)`", repl, text)


def _restore_inline_code(text):
    return re.sub(r"\x00CODE(\d+)\x00",
                  lambda m: f"<code>{_CODE_TOKENS[int(m.group(1))]}</code>", text)


def render_inline(text):
    _CODE_TOKENS.clear()
    text = _protect_inline_code(text)
    text = html.escape(text, quote=False)

    def wiki(m):
        inner = m.group(1)
        alias = None
        if "|" in inner:
            inner, alias = inner.split("|", 1)
        anchor = None
        if "#" in inner:
            inner, anchor = inner.split("#", 1)
        inner = inner.strip()
        anchor = anchor.strip() if anchor else None
        label = (alias or anchor or chapter_title(inner)).strip()

        if anchor:
            aslug = slugify(anchor)
            # same-page anchor (no file part) -> prefer current chapter
            if not inner:
                page_cid = CURRENT_CID if aslug in CHAPTER_HEADINGS.get(CURRENT_CID, set()) \
                    else HEADING_PAGE.get(aslug)
            else:
                page_cid = resolve_wiki_target(inner)
            if page_cid:
                return (f'<a class="wikilink" href="{page_filename(page_cid)}#{aslug}">'
                        f'{html.escape(label)}</a>')

        cid = resolve_wiki_target(inner) if inner else None
        if cid:
            return f'<a class="wikilink" href="{page_filename(cid)}">{html.escape(label)}</a>'
        return f'<span class="wikilink-dead">{html.escape(label)}</span>'

    text = re.sub(r"\[\[([^\]]+)\]\]", wiki, text)

    text = re.sub(
        r"\[([^\]]+)\]\(([^)]+)\)",
        lambda m: f'<a href="{html.escape(m.group(2))}" '
                  + ('target="_blank" rel="noopener"' if m.group(2).startswith("http") else "")
                  + f">{m.group(1)}</a>",
        text,
    )

    text = re.sub(r"\*\*\*([^*]+)\*\*\*", r"<strong><em>\1</em></strong>", text)
    text = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"(?<![\w*])\*(?!\s)([^*]+?)\*(?![\w*])", r"<em>\1</em>", text)
    text = re.sub(r"(?<![\w_])__([^_]+)__(?![\w_])", r"<strong>\1</strong>", text)

    return _restore_inline_code(text)


# ---------------------------------------------------------------------------
# block-level rendering
# ---------------------------------------------------------------------------

def render_code(lang, code):
    lang = (lang or "").strip().lower()
    if lang == "mermaid":
        return ('<div class="mermaid-wrap"><pre class="mermaid">'
                f'{html.escape(code)}</pre>'
                '<details class="mermaid-src"><summary>diagram source</summary>'
                f'<pre><code>{html.escape(code)}</code></pre></details></div>')
    label = lang if lang else "text"
    try:
        lexer = get_lexer_by_name(lang) if lang else get_lexer_by_name("text")
    except ClassNotFound:
        lexer = get_lexer_by_name("text")
    body = highlight(code, lexer, HtmlFormatter(nowrap=False, cssclass="hl"))
    return (f'<div class="code-block" data-lang="{html.escape(label)}">'
            f'<div class="code-bar"><span class="code-lang">{html.escape(label)}</span>'
            f'<button class="copy-btn" title="Copy">copy</button></div>{body}</div>')


CALLOUT_KINDS = {"note", "tip", "warning", "danger", "info", "important", "caution"}


def render_table(lines):
    def cells(row):
        row = row.strip()
        if row.startswith("|"):
            row = row[1:]
        if row.endswith("|"):
            row = row[:-1]
        return [c.strip() for c in row.split("|")]

    header = cells(lines[0])
    aligns = []
    for spec in cells(lines[1]):
        l, r = spec.startswith(":"), spec.endswith(":")
        aligns.append("center" if l and r else "right" if r else "left" if l else "")
    out = ['<div class="table-wrap"><table><thead><tr>']
    for i, h in enumerate(header):
        a = aligns[i] if i < len(aligns) else ""
        style = f' style="text-align:{a}"' if a else ""
        out.append(f"<th{style}>{render_inline(h)}</th>")
    out.append("</tr></thead><tbody>")
    for row in lines[2:]:
        if not row.strip():
            continue
        cs = cells(row)
        out.append("<tr>")
        for i, c in enumerate(cs):
            a = aligns[i] if i < len(aligns) else ""
            style = f' style="text-align:{a}"' if a else ""
            out.append(f"<td{style}>{render_inline(c)}</td>")
        out.append("</tr>")
    out.append("</tbody></table></div>")
    return "".join(out)


def render_list(items):
    html_out = []
    stack = []

    def close_to(indent):
        while stack and stack[-1][0] >= indent:
            _, tag = stack.pop()
            html_out.append(f"</li></{tag}>")

    for indent, mtype, content in items:
        tag = "ol" if mtype == "ol" else "ul"
        if not stack or indent > stack[-1][0]:
            html_out.append(f"<{tag}>")
            stack.append((indent, tag))
            html_out.append(f"<li>{render_inline(content)}")
        elif indent == stack[-1][0]:
            html_out.append(f"</li><li>{render_inline(content)}")
        else:
            close_to(indent)
            if stack and indent == stack[-1][0]:
                html_out.append(f"</li><li>{render_inline(content)}")
            else:
                html_out.append(f"<{tag}>")
                stack.append((indent, tag))
                html_out.append(f"<li>{render_inline(content)}")
    while stack:
        _, tag = stack.pop()
        html_out.append(f"</li></{tag}>")
    return "".join(html_out)


def parse_blocks(md, headings):
    lines = md.split("\n")
    out = []
    i, n = 0, len(md.split("\n"))

    while i < n:
        line = lines[i]
        if not line.strip():
            i += 1
            continue

        m = re.match(r"^```+\s*([\w+-]*)\s*$", line)
        if m:
            lang = m.group(1)
            j = i + 1
            buf = []
            while j < n and not re.match(r"^```+\s*$", lines[j]):
                buf.append(lines[j])
                j += 1
            out.append(render_code(lang, "\n".join(buf)))
            i = j + 1
            continue

        m = re.match(r"^(#{1,6})\s+(.*)$", line)
        if m:
            level = len(m.group(1))
            text = m.group(2).strip()
            hid = slugify(text)
            base, k = hid, 2
            existing = {h[2] for h in headings}
            while hid in existing:
                hid = f"{base}-{k}"
                k += 1
            headings.append((level, text, hid))
            out.append(f'<h{level} id="{hid}" class="anchored">'
                       f'<a class="hanchor" href="#{hid}" aria-label="link">#</a>'
                       f'{render_inline(text)}</h{level}>')
            i += 1
            continue

        if re.match(r"^(\*\s*){3,}$", line) or re.match(r"^(-\s*){3,}$", line) or re.match(r"^(_\s*){3,}$", line):
            out.append("<hr>")
            i += 1
            continue

        m = re.match(r"^>\s*\[!(\w+)\]\s*(.*)$", line)
        if m:
            kind = m.group(1).lower()
            if kind not in CALLOUT_KINDS:
                kind = "note"
            title = m.group(2).strip()
            j = i + 1
            body = []
            while j < n and lines[j].lstrip().startswith(">"):
                body.append(re.sub(r"^\s*>\s?", "", lines[j]))
                j += 1
            inner = parse_blocks("\n".join(body).strip(), headings) if body else ""
            head = f'<div class="callout-title">{render_inline(title) if title else kind.capitalize()}</div>'
            out.append(f'<div class="callout callout-{kind}">{head}{inner}</div>')
            i = j
            continue

        if line.lstrip().startswith(">"):
            j = i
            body = []
            while j < n and lines[j].lstrip().startswith(">"):
                body.append(re.sub(r"^\s*>\s?", "", lines[j]))
                j += 1
            out.append(f"<blockquote>{parse_blocks(chr(10).join(body).strip(), headings)}</blockquote>")
            i = j
            continue

        if "|" in line and i + 1 < n and re.match(r"^\s*\|?[\s:|-]+\|[\s:|-]*$", lines[i + 1]):
            j = i
            tbl = []
            while j < n and "|" in lines[j] and lines[j].strip():
                tbl.append(lines[j])
                j += 1
            out.append(render_table(tbl))
            i = j
            continue

        if re.match(r"^\s*([-*+]|\d+[.)])\s+", line):
            j = i
            items = []
            while j < n:
                lm = re.match(r"^(\s*)([-*+]|\d+[.)])\s+(.*)$", lines[j])
                if lm:
                    indent = len(lm.group(1).replace("\t", "    "))
                    mtype = "ol" if re.match(r"\d+[.)]", lm.group(2)) else "ul"
                    items.append((indent, mtype, lm.group(3)))
                    j += 1
                elif lines[j].strip() == "":
                    if j + 1 < n and re.match(r"^\s*([-*+]|\d+[.)])\s+", lines[j + 1]):
                        j += 1
                    else:
                        break
                elif lines[j].startswith(("    ", "\t")) and items:
                    items[-1] = (items[-1][0], items[-1][1], items[-1][2] + " " + lines[j].strip())
                    j += 1
                else:
                    break
            out.append(render_list(items))
            i = j
            continue

        j = i
        para = []
        while j < n and lines[j].strip() and not re.match(r"^(#{1,6}\s|```|>|\s*([-*+]|\d+[.)])\s)", lines[j]):
            if "|" in lines[j] and j + 1 < n and re.match(r"^\s*\|?[\s:|-]+\|[\s:|-]*$", lines[j + 1]):
                break
            para.append(lines[j])
            j += 1
        out.append(f"<p>{render_inline(' '.join(p.strip() for p in para))}</p>")
        i = j

    return "\n".join(out)


def strip_frontmatter(md):
    if md.startswith("---"):
        m = re.match(r"^---\n.*?\n---\n", md, re.DOTALL)
        if m:
            return md[m.end():]
    return md


def plain_text(md):
    """Crude markdown -> plain text for the search index."""
    md = strip_frontmatter(md)
    md = re.sub(r"```.*?```", " ", md, flags=re.DOTALL)
    md = re.sub(r"`([^`]+)`", r"\1", md)
    md = re.sub(r"\[\[([^\]|]+)(\|[^\]]+)?\]\]", lambda m: (m.group(2)[1:] if m.group(2) else m.group(1)), md)
    md = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", md)
    md = re.sub(r"[#>*_|`-]", " ", md)
    return re.sub(r"\s+", " ", md).strip()


# ---------------------------------------------------------------------------
# build
# ---------------------------------------------------------------------------

def get_summaries():
    """Extract the per-chapter one-line summaries from the index table."""
    path = os.path.join(HANDBOOK_DIR, INDEX_FILE)
    summaries = {}
    with open(path, encoding="utf-8") as fh:
        for ln in fh:
            m = re.match(r"^\|\s*(\d+)\s*\|\s*\[\[[^\]]+\]\]\s*\|\s*(.*?)\s*\|\s*$", ln)
            if m:
                summaries[m.group(1)] = m.group(2)
    return summaries


def main():
    global CURRENT_CID

    files = sorted(f for f in os.listdir(HANDBOOK_DIR)
                   if f.endswith(".md") and re.match(r"^\d+", f))
    order = [INDEX_FILE] + files
    summaries = get_summaries()

    # prepass: resolution maps + heading slugs per chapter
    for f in order:
        stem = f[:-3]
        cid = "ch-00" if f == INDEX_FILE else chapter_id(stem)
        WIKI_MAP[stem] = cid
        TITLE_MAP[chapter_title(stem).lower()] = cid
        CHAPTER_HEADINGS.setdefault(cid, set())
        with open(os.path.join(HANDBOOK_DIR, f), encoding="utf-8") as fh:
            for ln in fh:
                hm = re.match(r"^#{1,6}\s+(.*)$", ln)
                if hm:
                    s = slugify(hm.group(1).strip())
                    CHAPTER_HEADINGS[cid].add(s)
                    HEADING_PAGE.setdefault(s, cid)

    # parse all chapters
    chapters = []  # dict per chapter
    for f in order:
        stem = f[:-3]
        cid = "ch-00" if f == INDEX_FILE else chapter_id(stem)
        num = "00" if f == INDEX_FILE else (chapter_num(stem) or "00")
        title = "Overview & Index" if f == INDEX_FILE else chapter_title(stem)
        CURRENT_CID = cid
        with open(os.path.join(HANDBOOK_DIR, f), encoding="utf-8") as fh:
            raw = fh.read()
        md = strip_frontmatter(raw)
        headings = []
        body = parse_blocks(md, headings)
        chapters.append({
            "file": f, "cid": cid, "num": num, "title": title,
            "summary": summaries.get(num, ""), "body": body,
            "headings": headings, "text": plain_text(raw),
        })

    # write site
    if os.path.isdir(SITE_DIR):
        shutil.rmtree(SITE_DIR)
    os.makedirs(ASSET_DIR, exist_ok=True)

    with open(os.path.join(ASSET_DIR, "handbook.css"), "w", encoding="utf-8") as fh:
        fh.write(build_css())
    with open(os.path.join(ASSET_DIR, "handbook.js"), "w", encoding="utf-8") as fh:
        fh.write(JS)
    with open(os.path.join(ASSET_DIR, "search-index.js"), "w", encoding="utf-8") as fh:
        fh.write("window.SEARCH_INDEX=" + build_search_index(chapters) + ";")

    nav = build_sidebar(chapters)
    for idx, ch in enumerate(chapters):
        prev_ch = chapters[idx - 1] if idx > 0 else None
        nxt = chapters[idx + 1] if idx + 1 < len(chapters) else None
        page = render_page(ch, chapters, nav, prev_ch, nxt)
        with open(os.path.join(SITE_DIR, page_filename(ch["cid"])), "w", encoding="utf-8") as fh:
            fh.write(page)

    # remove the old single-file artifact if present
    old = os.path.join(HANDBOOK_DIR, "AgentOS-Handbook.html")
    if os.path.exists(old):
        os.remove(old)

    print(f"Wrote {len(chapters)} pages to {SITE_DIR}")
    print(f"Open: {os.path.join(SITE_DIR, 'index.html')}")


def build_search_index(chapters):
    records = []
    for ch in chapters:
        # one record per chapter (whole-text) + one per h2/h3 heading section
        records.append({
            "page": page_filename(ch["cid"]), "num": ch["num"],
            "title": ch["title"], "heading": "", "hid": "",
            "text": ch["text"][:4000],
        })
        text = ch["text"]
        for level, htext, hid in ch["headings"]:
            if level in (2, 3):
                # snippet starting at the heading text
                pos = text.find(htext)
                snip = text[pos:pos + 600] if pos >= 0 else htext
                records.append({
                    "page": page_filename(ch["cid"]), "num": ch["num"],
                    "title": ch["title"], "heading": htext, "hid": hid,
                    "text": snip,
                })
    return json.dumps(records, ensure_ascii=False)


def build_sidebar(chapters):
    """Sidebar listing all chapters (sub-sections injected per-page at render)."""
    rows = []
    for ch in chapters:
        if ch["cid"] == "ch-00":
            label = '<span class="ch-num">★</span> Overview'
        else:
            label = f'<span class="ch-num">{ch["num"]}</span> {html.escape(ch["title"])}'
        rows.append((ch["cid"], page_filename(ch["cid"]), label, ch))
    return rows


def render_sidebar_html(nav, active_cid, active_ch):
    out = ['<div class="nav-inner">',
           '<div class="nav-section-label">Handbook</div>']
    for cid, href, label, ch in nav:
        active = " active" if cid == active_cid else ""
        out.append(f'<div class="nav-chapter{" open" if cid == active_cid else ""}">')
        out.append(f'<a class="nav-link nav-top{active}" href="{href}">{label}</a>')
        if cid == active_cid:
            subs = [h for h in active_ch["headings"] if h[0] == 2]
            if subs:
                out.append('<div class="nav-subs">')
                for _l, text, hid in subs:
                    out.append(f'<a class="nav-sub" href="#{hid}" data-target="{hid}">{render_inline(text)}</a>')
                out.append("</div>")
        out.append("</div>")
    out.append("</div>")
    return "".join(out)


def render_toc(ch):
    subs = [h for h in ch["headings"] if h[0] in (2, 3)]
    if not subs:
        return ""
    out = ['<div class="toc"><div class="toc-title">On this page</div><ul>']
    for level, text, hid in subs:
        cls = "toc-h3" if level == 3 else "toc-h2"
        out.append(f'<li class="{cls}"><a href="#{hid}" data-target="{hid}">{render_inline(text)}</a></li>')
    out.append("</ul></div>")
    return "".join(out)


def render_landing_extra(chapters):
    """Hero + chapter card grid prepended to the index page body."""
    cards = []
    for ch in chapters:
        if ch["cid"] == "ch-00":
            continue
        cards.append(
            f'<a class="ch-card" href="{page_filename(ch["cid"])}">'
            f'<div class="ch-card-num">{ch["num"]}</div>'
            f'<div class="ch-card-title">{html.escape(ch["title"])}</div>'
            f'<div class="ch-card-sum">{html.escape(ch["summary"])}</div></a>'
        )
    return (
        '<div class="hero">'
        '<div class="hero-badge">LLM-native operating system</div>'
        '<h1 class="hero-title">AgentOS User Handbook</h1>'
        '<p class="hero-sub">The complete guide to installing, configuring, and operating AgentOS — '
        'where LLMs are the CPU, tools are the programs, and intent is the syscall. '
        'Every concept below is explained in depth with usage, examples, and the reasoning behind it.</p>'
        '<div class="hero-actions">'
        '<a class="btn btn-primary" href="ch-01.html">Start reading →</a>'
        '<a class="btn" href="ch-02.html">Install &amp; first run</a>'
        '<a class="btn" href="ch-04.html">CLI reference</a>'
        '</div></div>'
        '<h2 class="anchored" id="all-chapters">All chapters</h2>'
        f'<div class="card-grid">{"".join(cards)}</div>'
    )


def render_page(ch, chapters, nav, prev_ch, nxt):
    is_landing = ch["cid"] == "ch-00"
    sidebar = render_sidebar_html(nav, ch["cid"], ch)
    toc = render_toc(ch)

    if is_landing:
        header = ""
        body = render_landing_extra(chapters) + '<hr><h2 class="anchored" id="navigation-guide">Navigation guide</h2>' + ch["body"]
        crumb = "Overview"
        page_title = "AgentOS User Handbook"
    else:
        header = (f'<div class="ch-header"><div class="ch-kicker">Chapter {ch["num"]}</div>'
                  f'<h1 class="ch-title">{html.escape(ch["title"])}</h1>'
                  + (f'<p class="ch-summary">{html.escape(ch["summary"])}</p>' if ch["summary"] else "")
                  + "</div>")
        body = ch["body"]
        crumb = f'Chapter {ch["num"]} · {html.escape(ch["title"])}'
        page_title = f'{ch["title"]} · AgentOS Handbook'

    prev_link = (f'<a class="pn pn-prev" href="{page_filename(prev_ch["cid"])}">'
                 f'<span class="pn-dir">← Previous</span>'
                 f'<span class="pn-title">{html.escape("Overview" if prev_ch["cid"]=="ch-00" else prev_ch["title"])}</span></a>'
                 if prev_ch else '<span></span>')
    next_link = (f'<a class="pn pn-next" href="{page_filename(nxt["cid"])}">'
                 f'<span class="pn-dir">Next →</span>'
                 f'<span class="pn-title">{html.escape(nxt["title"])}</span></a>'
                 if nxt else '<span></span>')

    toc_aside = f'<aside id="toc-rail">{toc}</aside>' if toc else '<aside id="toc-rail"></aside>'

    return f"""<!DOCTYPE html>
<html lang="en" data-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(page_title)}</title>
<link rel="stylesheet" href="assets/handbook.css">
<script src="assets/search-index.js" defer></script>
</head>
<body>
<header id="topbar">
  <button id="menu-toggle" aria-label="Toggle navigation">☰</button>
  <a class="brand" href="index.html"><span class="logo">◆</span> AgentOS <span class="brand-sub">Handbook</span></a>
  <div class="topbar-spacer"></div>
  <div class="search-wrap">
    <input id="search" type="search" placeholder="Search the handbook…  (press /)" autocomplete="off">
    <div id="search-results" hidden></div>
  </div>
  <button id="theme-toggle" aria-label="Toggle theme" title="Toggle light/dark">🌙</button>
</header>
<div id="progress"></div>
<div id="layout">
  <nav id="sidebar">{sidebar}</nav>
  <div id="scrim"></div>
  <main id="content">
    <div class="breadcrumb"><a href="index.html">Handbook</a> <span>/</span> {crumb}</div>
    {header}
    <article class="prose">{body}</article>
    <nav class="pagenav">{prev_link}{next_link}</nav>
    <footer class="hb-footer">
      <p>AgentOS User Handbook · generated from <code>docs/handbook/*.md</code>.
      An LLM-native operating system for AI agents.</p>
    </footer>
  </main>
  {toc_aside}
</div>
<script src="https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.min.js" onerror="window.__noMermaid=true"></script>
<script src="assets/handbook.js" defer></script>
</body>
</html>"""


def build_css():
    pyg = HtmlFormatter(style="monokai", cssclass="hl").get_style_defs(".hl")
    pyg_light = HtmlFormatter(style="default", cssclass="hl").get_style_defs("[data-theme='light'] .hl")
    return CSS + "\n" + pyg + "\n" + pyg_light


# ---------------------------------------------------------------------------
# assets
# ---------------------------------------------------------------------------

CSS = r"""
:root{
  --bg:#0f1116; --bg-elev:#161922; --bg-soft:#1b1f2a; --border:#262b38;
  --fg:#e6e9ef; --fg-soft:#9aa3b2; --fg-dim:#6b7280;
  --accent:#6ea8fe; --accent-2:#7c5cff; --accent-soft:#1d2740;
  --code-bg:#11141b; --shadow:0 10px 40px rgba(0,0,0,.45);
  --sidebar-w:288px; --toc-w:228px; --topbar-h:56px;
  --note:#6ea8fe; --tip:#3fb950; --warning:#d29922; --danger:#f85149; --info:#56b6c2;
}
[data-theme='light']{
  --bg:#ffffff; --bg-elev:#f7f8fa; --bg-soft:#eef1f5; --border:#e1e5ec;
  --fg:#1c2128; --fg-soft:#57606a; --fg-dim:#8b949e;
  --accent:#0969da; --accent-2:#6639ba; --accent-soft:#ddf0ff;
  --code-bg:#f6f8fa; --shadow:0 10px 40px rgba(0,0,0,.10);
}
*{box-sizing:border-box}
html{scroll-behavior:smooth; scroll-padding-top:calc(var(--topbar-h) + 14px)}
body{margin:0; background:var(--bg); color:var(--fg);
  font:15px/1.65 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;
  -webkit-font-smoothing:antialiased}
a{color:var(--accent); text-decoration:none}

/* topbar */
#topbar{position:fixed; inset:0 0 auto 0; height:var(--topbar-h); z-index:40;
  display:flex; align-items:center; gap:12px; padding:0 16px;
  background:color-mix(in srgb,var(--bg-elev) 90%, transparent); backdrop-filter:blur(12px);
  border-bottom:1px solid var(--border)}
.brand{font-weight:700; display:flex; align-items:center; gap:8px; color:var(--fg)}
.brand .logo{color:var(--accent-2)}
.brand-sub{color:var(--fg-soft); font-weight:500}
.topbar-spacer{flex:1}
.search-wrap{position:relative}
#search{width:min(440px,44vw); padding:8px 12px; border-radius:9px; border:1px solid var(--border);
  background:var(--bg-soft); color:var(--fg); font-size:14px; outline:none; transition:border .15s,box-shadow .15s}
#search:focus{border-color:var(--accent); box-shadow:0 0 0 3px var(--accent-soft)}
#theme-toggle,#menu-toggle{background:var(--bg-soft); border:1px solid var(--border); color:var(--fg);
  width:38px; height:38px; border-radius:9px; cursor:pointer; font-size:16px; flex:none}
#menu-toggle{display:none}
#theme-toggle:hover,#menu-toggle:hover{border-color:var(--accent)}

/* search dropdown */
#search-results{position:absolute; top:46px; right:0; width:min(560px,86vw); max-height:70vh; overflow-y:auto;
  background:var(--bg-elev); border:1px solid var(--border); border-radius:12px; box-shadow:var(--shadow);
  padding:6px; z-index:60}
.sr-item{display:block; text-decoration:none; color:var(--fg); padding:10px 12px; border-radius:8px}
.sr-item:hover,.sr-item.sel{background:var(--bg-soft)}
.sr-ch{color:var(--accent-2); font-size:11.5px; font-weight:600}
.sr-head{font-weight:600; font-size:14px; margin:1px 0}
.sr-snip{color:var(--fg-soft); font-size:12.5px}
.sr-snip mark,.sr-head mark{background:var(--accent-soft); color:var(--accent); padding:0 2px; border-radius:3px}
.sr-empty{color:var(--fg-soft); padding:18px; text-align:center}

#progress{position:fixed; top:var(--topbar-h); left:0; height:3px; width:0; z-index:50;
  background:linear-gradient(90deg,var(--accent),var(--accent-2)); transition:width .1s}

/* layout */
#layout{display:grid; grid-template-columns:var(--sidebar-w) minmax(0,1fr) var(--toc-w);
  padding-top:var(--topbar-h)}
#sidebar{position:fixed; top:var(--topbar-h); bottom:0; left:0; width:var(--sidebar-w);
  overflow-y:auto; border-right:1px solid var(--border); background:var(--bg-elev); padding:14px 8px 50px}
.nav-section-label{font-size:11px; text-transform:uppercase; letter-spacing:.1em; color:var(--fg-dim);
  padding:6px 12px}
.nav-inner{display:flex; flex-direction:column; gap:1px}
.nav-link{display:block; text-decoration:none; color:var(--fg-soft); border-radius:7px;
  padding:7px 10px; font-size:13.5px; transition:background .12s,color .12s}
.nav-top{font-weight:600; color:var(--fg)}
.nav-link:hover{background:var(--bg-soft); color:var(--fg)}
.nav-link.active{background:var(--accent-soft); color:var(--accent)}
.ch-num{display:inline-block; min-width:22px; color:var(--fg-dim); font-variant-numeric:tabular-nums; font-size:12px}
.nav-link.active .ch-num{color:var(--accent)}
.nav-subs{margin:2px 0 6px 16px; border-left:1px solid var(--border); padding-left:6px; display:flex; flex-direction:column}
.nav-sub{text-decoration:none; color:var(--fg-dim); font-size:12.5px; padding:4px 9px; border-radius:6px}
.nav-sub:hover{color:var(--fg); background:var(--bg-soft)}
.nav-sub.active{color:var(--accent)}

/* content column */
#content{grid-column:2; min-width:0; padding:26px 48px 120px; max-width:900px; margin:0 auto; width:100%}
.breadcrumb{color:var(--fg-dim); font-size:12.5px; margin-bottom:18px}
.breadcrumb a{color:var(--fg-soft)}
.breadcrumb span{margin:0 6px}
.ch-header{border-bottom:1px solid var(--border); padding-bottom:18px; margin-bottom:8px}
.ch-kicker{color:var(--accent-2); font-weight:600; font-size:12.5px; letter-spacing:.12em; text-transform:uppercase}
.ch-title{font-size:32px; margin:8px 0 0; line-height:1.18}
.ch-summary{color:var(--fg-soft); font-size:16px; margin:12px 0 0}

/* prose */
.prose h1{font-size:26px; margin:36px 0 14px}
.prose h2{font-size:22px; margin:38px 0 14px; padding-bottom:7px; border-bottom:1px solid var(--border)}
.prose h3{font-size:17.5px; margin:26px 0 10px}
.prose h4{font-size:14px; margin:20px 0 8px; color:var(--fg-soft); text-transform:uppercase; letter-spacing:.05em}
.anchored{scroll-margin-top:calc(var(--topbar-h) + 14px); position:relative}
.hanchor{position:absolute; left:-22px; color:var(--fg-dim); text-decoration:none; opacity:0; transition:opacity .12s}
.anchored:hover .hanchor{opacity:1}
.prose p{margin:13px 0}
.prose a:hover{text-decoration:underline}
.wikilink{color:var(--accent-2); border-bottom:1px dotted var(--accent-2)}
.wikilink-dead{color:var(--fg-dim)}
.prose ul,.prose ol{margin:13px 0; padding-left:24px}
.prose li{margin:5px 0}
hr{border:none; border-top:1px solid var(--border); margin:30px 0}
strong{color:var(--fg)}
.prose code{background:var(--bg-soft); padding:.12em .4em; border-radius:5px; font-size:.88em;
  font-family:"SF Mono",ui-monospace,Menlo,Consolas,monospace; border:1px solid var(--border)}

/* tables */
.table-wrap{overflow-x:auto; margin:18px 0; border:1px solid var(--border); border-radius:10px}
table{border-collapse:collapse; width:100%; font-size:13.5px}
thead th{background:var(--bg-soft); text-align:left; font-weight:600; color:var(--fg);
  padding:10px 14px; border-bottom:1px solid var(--border); white-space:nowrap}
tbody td{padding:9px 14px; border-bottom:1px solid var(--border); vertical-align:top}
tbody tr:last-child td{border-bottom:none}
tbody tr:hover{background:var(--bg-soft)}
table code{white-space:nowrap}

/* code */
.code-block{margin:18px 0; border:1px solid var(--border); border-radius:10px; overflow:hidden; background:var(--code-bg)}
.code-bar{display:flex; align-items:center; justify-content:space-between; padding:6px 12px;
  background:var(--bg-soft); border-bottom:1px solid var(--border)}
.code-lang{font-size:11.5px; text-transform:uppercase; letter-spacing:.08em; color:var(--fg-dim); font-weight:600}
.copy-btn{background:transparent; border:1px solid var(--border); color:var(--fg-soft);
  font-size:11.5px; padding:3px 9px; border-radius:6px; cursor:pointer}
.copy-btn:hover{border-color:var(--accent); color:var(--accent)}
.copy-btn.copied{color:var(--tip); border-color:var(--tip)}
.hl{padding:14px 16px; overflow-x:auto; font-size:13px; line-height:1.55;
  font-family:"SF Mono",ui-monospace,Menlo,Consolas,monospace}
.hl pre{margin:0; background:transparent!important}

/* callouts */
.callout{margin:18px 0; border:1px solid var(--border); border-left-width:4px; border-radius:8px;
  padding:12px 16px; background:var(--bg-soft)}
.callout-title{font-weight:700; margin-bottom:6px; display:flex; align-items:center; gap:8px; font-size:14px}
.callout p:first-of-type{margin-top:0}.callout p:last-child{margin-bottom:0}
.callout-note{border-left-color:var(--note)} .callout-note .callout-title{color:var(--note)}
.callout-tip{border-left-color:var(--tip)} .callout-tip .callout-title{color:var(--tip)}
.callout-warning,.callout-caution{border-left-color:var(--warning)} .callout-warning .callout-title,.callout-caution .callout-title{color:var(--warning)}
.callout-danger{border-left-color:var(--danger)} .callout-danger .callout-title{color:var(--danger)}
.callout-info,.callout-important{border-left-color:var(--info)} .callout-info .callout-title,.callout-important .callout-title{color:var(--info)}
.callout-note .callout-title::before{content:"ℹ️"}
.callout-tip .callout-title::before{content:"💡"}
.callout-warning .callout-title::before,.callout-caution .callout-title::before{content:"⚠️"}
.callout-danger .callout-title::before{content:"🛑"}
.callout-info .callout-title::before,.callout-important .callout-title::before{content:"📌"}
blockquote{margin:18px 0; padding:4px 16px; border-left:4px solid var(--border); color:var(--fg-soft)}

/* mermaid */
.mermaid-wrap{margin:18px 0; background:var(--bg-soft); border:1px solid var(--border); border-radius:10px; padding:16px; text-align:center}
.mermaid{overflow-x:auto}
.mermaid-src{margin-top:8px; text-align:left}
.mermaid-src summary{cursor:pointer; color:var(--fg-dim); font-size:12px}

/* landing */
.hero{padding:24px 0 8px}
.hero-badge{display:inline-block; font-size:12px; font-weight:600; color:var(--accent-2);
  background:var(--accent-soft); padding:5px 12px; border-radius:999px; letter-spacing:.03em}
.hero-title{font-size:42px; line-height:1.1; margin:16px 0 0;
  background:linear-gradient(90deg,var(--fg),var(--accent)); -webkit-background-clip:text; background-clip:text; -webkit-text-fill-color:transparent}
.hero-sub{font-size:17px; color:var(--fg-soft); max-width:680px; margin:16px 0 0}
.hero-actions{display:flex; flex-wrap:wrap; gap:10px; margin-top:22px}
.btn{display:inline-block; padding:9px 16px; border-radius:9px; border:1px solid var(--border);
  color:var(--fg); font-weight:600; font-size:14px; background:var(--bg-soft)}
.btn:hover{border-color:var(--accent)}
.btn-primary{background:linear-gradient(90deg,var(--accent),var(--accent-2)); color:#fff; border:none}
.card-grid{display:grid; grid-template-columns:repeat(auto-fill,minmax(230px,1fr)); gap:14px; margin:18px 0 8px}
.ch-card{display:block; text-decoration:none; color:var(--fg); border:1px solid var(--border); border-radius:12px;
  padding:16px; background:var(--bg-elev); transition:transform .12s,border-color .12s,box-shadow .12s}
.ch-card:hover{transform:translateY(-3px); border-color:var(--accent); box-shadow:var(--shadow)}
.ch-card-num{font-size:12px; font-weight:700; color:var(--accent-2); font-variant-numeric:tabular-nums}
.ch-card-title{font-size:16px; font-weight:700; margin:4px 0 6px}
.ch-card-sum{font-size:12.5px; color:var(--fg-soft); line-height:1.5}

/* prev/next + toc */
.pagenav{display:flex; justify-content:space-between; gap:14px; margin:48px 0 0}
.pn{display:flex; flex-direction:column; gap:3px; max-width:48%; padding:14px 16px; border:1px solid var(--border);
  border-radius:11px; background:var(--bg-elev); text-decoration:none}
.pn:hover{border-color:var(--accent)}
.pn-next{align-items:flex-end; text-align:right; margin-left:auto}
.pn-dir{font-size:12px; color:var(--fg-dim)}
.pn-title{font-weight:600; color:var(--fg)}
.hb-footer{margin-top:54px; padding-top:20px; border-top:1px solid var(--border); color:var(--fg-dim); font-size:13px}

#toc-rail{grid-column:3; position:sticky; top:calc(var(--topbar-h) + 18px); align-self:start;
  height:calc(100vh - var(--topbar-h) - 36px); overflow-y:auto; padding:26px 18px}
.toc-title{font-size:11px; text-transform:uppercase; letter-spacing:.1em; color:var(--fg-dim); margin-bottom:10px}
.toc ul{list-style:none; margin:0; padding:0; border-left:1px solid var(--border)}
.toc li{margin:0}
.toc a{display:block; color:var(--fg-dim); text-decoration:none; font-size:12.5px; padding:4px 12px;
  border-left:2px solid transparent; margin-left:-1px}
.toc a:hover{color:var(--fg)}
.toc a.active{color:var(--accent); border-left-color:var(--accent)}
.toc-h3 a{padding-left:24px; font-size:12px}

#scrim{display:none}
@media(max-width:1180px){
  #layout{grid-template-columns:var(--sidebar-w) minmax(0,1fr)}
  #toc-rail{display:none}
  #content{grid-column:2}
}
@media(max-width:900px){
  #menu-toggle{display:block}
  #layout{grid-template-columns:1fr}
  #sidebar{transform:translateX(-100%); transition:transform .22s; z-index:35; width:84vw; max-width:320px}
  body.nav-open #sidebar{transform:none; box-shadow:var(--shadow)}
  body.nav-open #scrim{display:block; position:fixed; inset:var(--topbar-h) 0 0 0; background:rgba(0,0,0,.5); z-index:34}
  #content{grid-column:1; padding:20px 18px 100px}
  #search{width:46vw}.brand-sub{display:none}
  .hero-title{font-size:32px}
}
"""

JS = r"""
const $=(s,r=document)=>r.querySelector(s);
const $$=(s,r=document)=>[...r.querySelectorAll(s)];

/* theme */
const themeBtn=$('#theme-toggle');
const setTheme=t=>{document.documentElement.dataset.theme=t; themeBtn.textContent=t==='dark'?'🌙':'☀️';
  try{localStorage.setItem('hb-theme',t)}catch(e){}
  if(window.mermaid){try{document.querySelectorAll('.mermaid[data-processed]').length}catch(e){}}};
setTheme((()=>{try{return localStorage.getItem('hb-theme')||'dark'}catch(e){return 'dark'}})());
themeBtn.onclick=()=>setTheme(document.documentElement.dataset.theme==='dark'?'light':'dark');

/* mobile nav */
const toggle=$('#menu-toggle');
toggle&&(toggle.onclick=()=>document.body.classList.toggle('nav-open'));
$('#scrim').onclick=()=>document.body.classList.remove('nav-open');
$$('#sidebar a').forEach(a=>a.addEventListener('click',()=>document.body.classList.remove('nav-open')));

/* copy buttons */
$$('.copy-btn').forEach(btn=>btn.onclick=()=>{
  const code=btn.closest('.code-block').querySelector('.hl').innerText;
  navigator.clipboard.writeText(code).then(()=>{btn.textContent='copied';btn.classList.add('copied');
    setTimeout(()=>{btn.textContent='copy';btn.classList.remove('copied')},1400)});
});

/* reading progress */
addEventListener('scroll',()=>{const h=document.documentElement;const max=h.scrollHeight-h.clientHeight;
  $('#progress').style.width=(max>0?h.scrollTop/max*100:0)+'%';},{passive:true});

/* scroll-spy for TOC + sidebar subsections */
const tocLinks=$$('#toc-rail a, .nav-sub');
const heads=$$('.prose h2[id], .prose h3[id]');
if(heads.length){
  const spy=new IntersectionObserver(es=>{
    es.forEach(e=>{ if(!e.isIntersecting)return;
      tocLinks.forEach(a=>a.classList.toggle('active', a.getAttribute('data-target')===e.target.id));
    });
  },{rootMargin:'-12% 0px -78% 0px',threshold:0});
  heads.forEach(h=>spy.observe(h));
}

/* ---- cross-page search ---- */
const search=$('#search'), results=$('#search-results');
const INDEX=window.SEARCH_INDEX||[];
function esc(s){return s.replace(/[.*+?^${}()|[\]\\]/g,'\\$&');}
function mark(text,q){return text.replace(new RegExp(esc(q),'ig'),m=>`<mark>${m}</mark>`);}
function snippet(text,q){
  const i=text.toLowerCase().indexOf(q.toLowerCase());
  if(i<0)return text.slice(0,130);
  const s=Math.max(0,i-45);
  return (s>0?'…':'')+mark(text.slice(s,i+q.length+95),q)+'…';
}
let sel=-1, items=[];
function run(){
  const q=search.value.trim();
  if(q.length<2){results.hidden=true; return;}
  const ql=q.toLowerCase();
  const hits=[];
  for(const r of INDEX){
    const inHead=r.heading&&r.heading.toLowerCase().includes(ql);
    const inText=r.text.toLowerCase().includes(ql);
    if(inHead||inText){
      hits.push({...r, score:(inHead?2:0)+(r.heading?0:1)*0 + (inText?1:0)});
    }
  }
  hits.sort((a,b)=>b.score-a.score);
  const seen=new Set(); const uniq=[];
  for(const h of hits){const key=h.page+'#'+h.hid; if(seen.has(key))continue; seen.add(key); uniq.push(h); if(uniq.length>=30)break;}
  items=uniq; sel=-1;
  if(!uniq.length){results.innerHTML=`<div class="sr-empty">No matches for “${q}”.</div>`;}
  else{results.innerHTML=uniq.map((h,i)=>{
    const href=h.hid?`${h.page}#${h.hid}`:h.page;
    const head=h.heading?mark(h.heading,q):'Chapter overview';
    return `<a class="sr-item" data-i="${i}" href="${href}">
      <div class="sr-ch">Ch ${h.num} · ${h.title}</div>
      <div class="sr-head">${head}</div>
      <div class="sr-snip">${snippet(h.text,q)}</div></a>`;}).join('');}
  results.hidden=false;
}
let t; search&&search.addEventListener('input',()=>{clearTimeout(t);t=setTimeout(run,110);});
search&&search.addEventListener('focus',()=>{if(search.value.trim().length>=2)run();});
document.addEventListener('click',e=>{if(!e.target.closest('.search-wrap'))results.hidden=true;});
search&&search.addEventListener('keydown',e=>{
  if(results.hidden)return;
  if(e.key==='ArrowDown'){e.preventDefault();sel=Math.min(sel+1,items.length-1);}
  else if(e.key==='ArrowUp'){e.preventDefault();sel=Math.max(sel-1,0);}
  else if(e.key==='Enter'){const el=results.querySelector(`[data-i="${sel}"]`)||results.querySelector('.sr-item');if(el)location.href=el.getAttribute('href');return;}
  else return;
  $$('.sr-item',results).forEach(a=>a.classList.toggle('sel',+a.dataset.i===sel));
  const cur=results.querySelector('.sr-item.sel'); if(cur)cur.scrollIntoView({block:'nearest'});
});
document.addEventListener('keydown',e=>{
  if(e.key==='/'&&document.activeElement!==search){e.preventDefault();search.focus();}
  if(e.key==='Escape'){results.hidden=true;if(document.activeElement===search)search.blur();}
});

/* mermaid */
if(!window.__noMermaid&&window.mermaid){
  try{mermaid.initialize({startOnLoad:true,theme:document.documentElement.dataset.theme==='dark'?'dark':'default'});}catch(e){}
}
"""


if __name__ == "__main__":
    main()
