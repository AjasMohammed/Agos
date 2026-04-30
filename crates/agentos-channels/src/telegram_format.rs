//! Markdown → Telegram HTML converter.
//!
//! Telegram's `parse_mode: "HTML"` accepts a small subset of HTML:
//! `<b>`, `<i>`, `<u>`, `<s>`, `<code>`, `<pre>`, `<a href>`, `<blockquote>`,
//! `<tg-spoiler>`. Outside tag context, `&`, `<`, `>` must be escaped.
//!
//! Strategy:
//! 1. Escape `&`, `<`, `>` everywhere first — guarantees valid Telegram HTML
//!    even when agent output contains stray angle brackets, "5 < 3", etc.
//! 2. Extract fenced code blocks (```...```) and inline code (`code`) into
//!    placeholders so their contents skip further markdown transforms.
//! 3. Apply transforms in priority order: `**bold**`, `*italic*`/`_italic_`,
//!    `~~strike~~`, `[label](url)`.
//! 4. Restore code placeholders as `<code>`/`<pre>` blocks.
//!
//! The output is always well-formed HTML accepted by Telegram's parser.
//! Unmatched markers (e.g. a single stray `*`) are left as plain text.

use std::borrow::Cow;

/// Convert markdown-flavoured text to Telegram HTML (parse_mode: HTML).
pub fn markdown_to_telegram_html(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    let escaped = escape_html(input);

    let mut placeholders: Vec<String> = Vec::new();
    let with_pre = extract_fenced_blocks(&escaped, &mut placeholders);
    let with_code = extract_inline_code(&with_pre, &mut placeholders);

    let with_bold = replace_paired(&with_code, "**", "<b>", "</b>");
    let with_strike = replace_paired(&with_bold, "~~", "<s>", "</s>");
    let with_italic = replace_italic(&with_strike);
    let with_links = replace_links(&with_italic);

    restore_placeholders(&with_links, &placeholders)
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

const PLACEHOLDER_PREFIX: &str = "\u{0001}TGPH";
const PLACEHOLDER_SUFFIX: &str = "\u{0001}";

fn make_placeholder(idx: usize) -> String {
    format!("{PLACEHOLDER_PREFIX}{idx}{PLACEHOLDER_SUFFIX}")
}

/// Extract ```...``` fenced blocks and replace with placeholders. Optional
/// language hint after opening fence becomes `<pre><code class="language-…">`.
fn extract_fenced_blocks(input: &str, placeholders: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"```") {
            let after_fence = i + 3;
            // Optional language identifier on the same line.
            let mut lang_end = after_fence;
            while lang_end < bytes.len() && bytes[lang_end] != b'\n' {
                lang_end += 1;
            }
            let lang: &str = std::str::from_utf8(&bytes[after_fence..lang_end])
                .unwrap_or("")
                .trim();
            let body_start = (lang_end + 1).min(bytes.len());

            // Find closing ```.
            if let Some(rel) = find_subslice(&bytes[body_start..], b"```") {
                let body_end = body_start + rel;
                let body = std::str::from_utf8(&bytes[body_start..body_end]).unwrap_or("");
                let body = body.trim_end_matches('\n');
                let html = if lang.is_empty() {
                    format!("<pre>{body}</pre>")
                } else {
                    format!(
                        "<pre><code class=\"language-{}\">{}</code></pre>",
                        sanitize_lang(lang),
                        body
                    )
                };
                placeholders.push(html);
                out.push_str(&make_placeholder(placeholders.len() - 1));
                i = body_end + 3;
                continue;
            }
            // No closing fence — treat as literal.
        }
        let ch = input[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn sanitize_lang(lang: &str) -> String {
    lang.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '+')
        .take(32)
        .collect()
}

/// Replace `` `code` `` with `<code>...</code>` placeholders.
fn extract_inline_code(input: &str, placeholders: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            // Find matching closing backtick on the same logical chunk.
            if let Some(rel) = chars[i + 1..].iter().position(|c| *c == '`') {
                let body: String = chars[i + 1..i + 1 + rel].iter().collect();
                if !body.is_empty() {
                    let html = format!("<code>{body}</code>");
                    placeholders.push(html);
                    out.push_str(&make_placeholder(placeholders.len() - 1));
                    i += 1 + rel + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Replace `<delim>text<delim>` pairs with `<open>text<close>`.
fn replace_paired(input: &str, delim: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(delim) {
        let after = start + delim.len();
        if let Some(rel_end) = rest[after..].find(delim) {
            // Disallow empty pair and unbroken whitespace-only pair.
            let inner = &rest[after..after + rel_end];
            if inner.is_empty() || inner.starts_with(char::is_whitespace) {
                out.push_str(&rest[..after]);
                rest = &rest[after..];
                continue;
            }
            out.push_str(&rest[..start]);
            out.push_str(open);
            out.push_str(inner);
            out.push_str(close);
            rest = &rest[after + rel_end + delim.len()..];
        } else {
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Italic: matches `*x*` or `_x_` where the marker is not adjacent to another
/// of the same kind (so `**` and `__` already-replaced bold survive).
fn replace_italic(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '*' || c == '_') && !is_adjacent_same(&chars, i, c) {
            // Find matching close, skipping pairs of the same marker.
            let mut j = i + 1;
            let mut close = None;
            while j < chars.len() {
                if chars[j] == c && !is_adjacent_same(&chars, j, c) {
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(end) = close {
                let inner: String = chars[i + 1..end].iter().collect();
                if !inner.is_empty() && !inner.starts_with(char::is_whitespace) {
                    out.push_str("<i>");
                    out.push_str(&inner);
                    out.push_str("</i>");
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn is_adjacent_same(chars: &[char], i: usize, c: char) -> bool {
    let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
    let next = chars.get(i + 1).copied();
    prev == Some(c) || next == Some(c)
}

/// Replace markdown links `[label](url)` with `<a href="url">label</a>`.
/// URLs containing nested parens are not supported — kept simple on purpose.
fn replace_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(label_end) = find_unescaped(&bytes[i + 1..], b']') {
                let after_label = i + 1 + label_end + 1;
                if after_label < bytes.len() && bytes[after_label] == b'(' {
                    let url_start = after_label + 1;
                    if let Some(url_end_rel) = find_unescaped(&bytes[url_start..], b')') {
                        let url_end = url_start + url_end_rel;
                        let label =
                            std::str::from_utf8(&bytes[i + 1..i + 1 + label_end]).unwrap_or("");
                        let url = std::str::from_utf8(&bytes[url_start..url_end]).unwrap_or("");
                        if is_safe_url(url) {
                            out.push_str(&format!(
                                "<a href=\"{}\">{}</a>",
                                attr_escape(url),
                                label
                            ));
                            i = url_end + 1;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn find_unescaped(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|b| *b == needle)
}

fn is_safe_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("tg://")
        || lower.starts_with("mailto:")
}

fn attr_escape(url: &str) -> Cow<'_, str> {
    if url.contains('"') {
        Cow::Owned(url.replace('"', "&quot;"))
    } else {
        Cow::Borrowed(url)
    }
}

fn restore_placeholders(input: &str, placeholders: &[String]) -> String {
    let mut out = input.to_string();
    for (idx, html) in placeholders.iter().enumerate() {
        let key = make_placeholder(idx);
        out = out.replace(&key, html);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_escaped_only() {
        assert_eq!(
            markdown_to_telegram_html("5 < 3 & 6 > 1"),
            "5 &lt; 3 &amp; 6 &gt; 1"
        );
    }

    #[test]
    fn bold_renders() {
        assert_eq!(markdown_to_telegram_html("**hi**"), "<b>hi</b>");
    }

    #[test]
    fn italic_star_renders() {
        assert_eq!(markdown_to_telegram_html("*hi*"), "<i>hi</i>");
    }

    #[test]
    fn italic_underscore_renders() {
        assert_eq!(markdown_to_telegram_html("_hi_"), "<i>hi</i>");
    }

    #[test]
    fn bold_then_italic_compose() {
        assert_eq!(
            markdown_to_telegram_html("**bold** and *italic*"),
            "<b>bold</b> and <i>italic</i>"
        );
    }

    #[test]
    fn strikethrough_renders() {
        assert_eq!(markdown_to_telegram_html("~~gone~~"), "<s>gone</s>");
    }

    #[test]
    fn inline_code_renders() {
        assert_eq!(
            markdown_to_telegram_html("run `ls -la`"),
            "run <code>ls -la</code>"
        );
    }

    #[test]
    fn fenced_code_renders() {
        let s = "```\nfn main() {}\n```";
        assert!(markdown_to_telegram_html(s).contains("<pre>fn main() {}</pre>"));
    }

    #[test]
    fn fenced_with_language() {
        let s = "```rust\nlet x = 1;\n```";
        let html = markdown_to_telegram_html(s);
        assert!(html.contains("<pre><code class=\"language-rust\">let x = 1;</code></pre>"));
    }

    #[test]
    fn link_renders_with_safe_scheme() {
        assert_eq!(
            markdown_to_telegram_html("[ex](https://example.com)"),
            "<a href=\"https://example.com\">ex</a>"
        );
    }

    #[test]
    fn link_with_dangerous_scheme_left_alone() {
        let out = markdown_to_telegram_html("[x](javascript:alert(1))");
        assert!(out.contains("[x](javascript:alert(1))"));
    }

    #[test]
    fn code_content_is_html_escaped() {
        let html = markdown_to_telegram_html("`<script>`");
        assert_eq!(html, "<code>&lt;script&gt;</code>");
    }

    #[test]
    fn unmatched_markers_passthrough() {
        // Whitespace-padded markers do not italicize (CommonMark-ish).
        assert_eq!(markdown_to_telegram_html("a * b * c"), "a * b * c");
        assert_eq!(markdown_to_telegram_html("a * b"), "a * b");
    }

    #[test]
    fn empty_input() {
        assert_eq!(markdown_to_telegram_html(""), "");
    }

    #[test]
    fn ampersand_in_url_escaped_in_attr() {
        let html = markdown_to_telegram_html("[s](https://x.com/?a=1&b=2)");
        assert!(html.contains("href=\"https://x.com/?a=1&amp;b=2\""));
    }
}
