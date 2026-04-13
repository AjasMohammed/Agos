//! Server-side sanitization of LLM output to prevent internal tool-call
//! scaffolding from leaking into user-visible chat text.
//!
//! The kernel's documented tool-calling protocol (see [`crate::system_prompt`])
//! tells models to emit fenced ```json blocks of the form
//! `{"tool": "...", "intent_type": "...", "payload": {...}}`. Most LLM adapters
//! parse these into the structured `result.tool_calls` channel. When an adapter
//! misses one (or a model emits the block in its visible text instead of the
//! native tool-use channel), the JSON would otherwise render as visible text in
//! the chat UI and the tool would never run.
//!
//! This module provides:
//!
//! 1. [`extract_tool_intent_blocks`] — a complete-text extractor that strips
//!    matching fenced blocks from a string and returns the parsed intents so
//!    the caller can promote them into `result.tool_calls`.
//! 2. [`OutputSanitizerStream`] — a stateful streaming filter that hides
//!    matching blocks from streamed text chunks before they reach the user.
//!    The filter is hide-only; it does not execute tool calls. Pair it with
//!    [`extract_tool_intent_blocks`] on the post-stream `result.text` to
//!    actually execute the leaked intents.
//!
//! Both functions are conservative: only fenced blocks whose body parses as
//! one-or-more JSON objects of the strict tool-intent shape are touched.
//! Tutorial code samples that happen to be ```json blocks containing
//! unrelated JSON are left alone.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool-call intent extracted from a fenced ```json block in LLM output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedToolIntent {
    pub tool: String,
    pub intent_type: String,
    pub payload: Value,
}

/// Result of extracting fenced tool-intent blocks from a complete text.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionResult {
    /// Text with all matched blocks removed.
    pub cleaned_text: String,
    /// Tool intents extracted from removed blocks, in source order.
    pub extracted: Vec<ExtractedToolIntent>,
}

/// Controls which output sanitization passes are applied.
///
/// Mirrors OpenClaw's `AssistantVisibleTextSanitizerProfile` pattern where
/// different consumers (live delivery, stored history, developer debug) see
/// the same text through different filter pipelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SanitizeProfile {
    /// Maximum filtering for user-facing output (SSE stream, persisted chat
    /// messages, `ChatInferenceResult::answer`). Runs all passes: fenced-block
    /// extraction, XML tag stripping, optional `<final>` enforcement, and
    /// error-payload rewriting.
    Delivery,
    /// For the assistant context-window entry that feeds the next LLM turn.
    /// Strips fenced tool blocks and XML tool tags (to avoid re-tempting the
    /// model to repeat the leaked format), but preserves reasoning prose, does
    /// not enforce `<final>`, and does not rewrite errors (the model should
    /// see raw errors so it can reason about retrying).
    History,
    /// Minimal filtering for kernel tracing at debug level and developer
    /// inspection. Only strips fenced tool blocks so they don't clutter logs,
    /// but preserves XML tags, reasoning, errors, and everything else.
    Debug,
}

/// Result of running the sanitization pipeline via [`sanitize_visible_text`].
#[derive(Debug, Clone)]
pub struct SanitizeResult {
    /// The cleaned text for the given profile.
    pub text: String,
    /// Tool intents extracted from fenced ` ```json ` blocks. Present for all
    /// profiles because the extraction is always the first pass.
    pub extracted_intents: Vec<ExtractedToolIntent>,
}

/// Run the complete output sanitization pipeline for `profile` on `text`.
///
/// Pass ordering (each step feeds the next):
///
/// 1. **Fenced-block extraction** ([`extract_tool_intent_blocks`]) — always runs.
///    Returns extracted tool intents + cleaned text with the blocks removed.
/// 2. **XML tool-tag stripping** ([`strip_xml_tool_tags`]) — runs for
///    `Delivery` and `History` profiles. Strips `<tool_call>`, `<invoke>`, etc.
/// 3. **`<final>` enforcement** ([`FinalTagFilter`]) — runs for `Delivery`
///    only, and only when `enforce_final_tag` is true.
/// 4. **Error-payload rewriting** ([`rewrite_error_payload`]) — runs for
///    `Delivery` only.
pub fn sanitize_visible_text(
    text: &str,
    profile: SanitizeProfile,
    enforce_final_tag: bool,
) -> SanitizeResult {
    let input_len = text.len();

    // Step 1: always extract fenced tool-intent blocks.
    let extraction = extract_tool_intent_blocks(text);
    let mut cleaned = extraction.cleaned_text;
    let after_step1 = cleaned.len();

    // Step 2: XML stripping for Delivery and History.
    if profile == SanitizeProfile::Delivery || profile == SanitizeProfile::History {
        cleaned = strip_xml_tool_tags(&cleaned);
    }
    let after_step2 = cleaned.len();

    // Step 3: <final> enforcement for Delivery only (when enabled).
    // When <final> enforcement is on, FinalTagFilter handles BOTH <final>
    // gating and <think> stripping in a single pass. When it is off, we
    // still strip <think> blocks (Step 3b) so models that emit reasoning
    // markers (Claude, DeepSeek, etc.) don't leak them to the user.
    if profile == SanitizeProfile::Delivery && enforce_final_tag {
        let mut filter = FinalTagFilter::new();
        let mut out = filter.push(&cleaned);
        out.push_str(&filter.flush());
        cleaned = out;
    } else if profile == SanitizeProfile::Delivery {
        // Step 3b: standalone <think> stripping without <final> enforcement.
        cleaned = strip_think_tags(&cleaned);
    }
    let after_step3 = cleaned.len();

    // Step 4: error-payload rewriting for Delivery only.
    if profile == SanitizeProfile::Delivery {
        if let Some(rewritten) = rewrite_error_payload(&cleaned) {
            cleaned = rewritten;
        }
    }

    // Log when significant content was stripped (helps diagnose empty responses).
    if input_len > 0 && cleaned.trim().is_empty() {
        tracing::debug!(
            profile = ?profile,
            input_len = input_len,
            after_fenced_extraction = after_step1,
            after_xml_strip = after_step2,
            after_think_final = after_step3,
            final_len = cleaned.len(),
            extracted_intents = extraction.extracted.len(),
            enforce_final_tag = enforce_final_tag,
            "Sanitizer reduced non-empty input to empty output"
        );
    }

    SanitizeResult {
        text: cleaned,
        extracted_intents: extraction.extracted,
    }
}

/// Scan a complete text for fenced ```json blocks whose contents parse as
/// AgentOS tool intents and remove them. Each removed block contributes its
/// parsed intents to the returned [`ExtractionResult::extracted`] list.
///
/// A block is removed only when **every** JSON value inside it is a valid
/// tool intent (object with `tool: string`, `intent_type: string`,
/// `payload: any`). Mixed blocks and non-tool JSON blocks are left in place.
pub fn extract_tool_intent_blocks(text: &str) -> ExtractionResult {
    let mut cleaned = String::with_capacity(text.len());
    let mut extracted = Vec::new();
    let mut cursor = 0;

    loop {
        let open_pos = match find_triple_backtick(text, cursor) {
            Some(p) => p,
            None => {
                cleaned.push_str(&text[cursor..]);
                break;
            }
        };

        cleaned.push_str(&text[cursor..open_pos]);

        let body_start = match consume_lang_tag_complete(text, open_pos + 3) {
            Some(b) => b,
            None => {
                cleaned.push_str(&text[open_pos..]);
                break;
            }
        };

        let close_pos = match find_triple_backtick(text, body_start) {
            Some(p) => p,
            None => {
                cleaned.push_str(&text[open_pos..]);
                break;
            }
        };

        let body = &text[body_start..close_pos];
        match parse_intent_body(body) {
            Some(intents) => {
                extracted.extend(intents);
                // Trim a trailing newline from `cleaned` and skip a leading
                // newline after the closing fence so the prose doesn't sprout
                // an extra blank line where the block used to be.
                if cleaned.ends_with('\n') {
                    cleaned.pop();
                    if cleaned.ends_with('\r') {
                        cleaned.pop();
                    }
                }
                let after = close_pos + 3;
                let mut skip_to = after;
                let bytes = text.as_bytes();
                if skip_to < bytes.len() && bytes[skip_to] == b'\r' {
                    skip_to += 1;
                }
                if skip_to < bytes.len() && bytes[skip_to] == b'\n' {
                    skip_to += 1;
                }
                cursor = skip_to;
            }
            None => {
                cleaned.push_str(&text[open_pos..close_pos + 3]);
                cursor = close_pos + 3;
            }
        }
    }

    ExtractionResult {
        cleaned_text: cleaned,
        extracted,
    }
}

/// Stateful streaming filter that hides fenced tool-intent blocks from streamed
/// text chunks. Buffers across chunk boundaries so a fence opened in one chunk
/// and closed in another is handled correctly.
///
/// Hide-only — does **not** execute tool calls. Use [`extract_tool_intent_blocks`]
/// on the complete `result.text` after streaming finishes to promote leaked
/// intents into `result.tool_calls`.
pub struct OutputSanitizerStream {
    pending: String,
    suppressed: usize,
}

impl OutputSanitizerStream {
    pub fn new() -> Self {
        Self {
            pending: String::new(),
            suppressed: 0,
        }
    }

    /// Push the next chunk of text. Returns the portion that should be emitted
    /// to the user. May be empty if all current content is buffered inside an
    /// open fence or trailing partial fence delimiter.
    pub fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let text = std::mem::take(&mut self.pending);
        let (output, hold) = self.process_owned(text);
        self.pending = hold;
        output
    }

    /// Flush any remaining buffered content. Call exactly once after the
    /// stream ends. If the stream ended with an unclosed fence, the buffered
    /// content is emitted as-is — we cannot tell whether it would have parsed
    /// as a tool block, and silently dropping is worse than showing a partial.
    pub fn flush(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }

    /// Number of fenced blocks suppressed during this stream.
    pub fn suppressed_count(&self) -> usize {
        self.suppressed
    }

    fn process_owned(&mut self, text: String) -> (String, String) {
        let mut output = String::new();
        let mut cursor = 0;

        loop {
            let open_pos = match find_triple_backtick(&text, cursor) {
                Some(p) => p,
                None => {
                    let tail = trailing_backtick_count(&text[cursor..]);
                    let emit_end = text.len() - tail;
                    output.push_str(&text[cursor..emit_end]);
                    return (output, text[emit_end..].to_string());
                }
            };

            output.push_str(&text[cursor..open_pos]);

            let body_start = match consume_lang_tag_complete(&text, open_pos + 3) {
                Some(b) => b,
                None => return (output, text[open_pos..].to_string()),
            };

            let close_pos = match find_triple_backtick(&text, body_start) {
                Some(p) => p,
                None => return (output, text[open_pos..].to_string()),
            };

            let body = &text[body_start..close_pos];
            if parse_intent_body(body).is_some() {
                self.suppressed += 1;
            } else {
                output.push_str(&text[open_pos..close_pos + 3]);
            }
            cursor = close_pos + 3;
        }
    }
}

impl Default for OutputSanitizerStream {
    fn default() -> Self {
        Self::new()
    }
}

// ── FinalTagFilter ──────────────────────────────────────────────────────────

/// Stateful streaming filter that enforces `<final>...</final>` wrapping on
/// LLM output. Inspired by OpenClaw's `pi-embedded-subscribe.ts` strict-mode
/// final-tag enforcement.
///
/// **Emission rule:** a character is emitted iff `in_final && !in_think`.
/// Content outside `<final>` blocks (model "thinking out loud") and content
/// inside `<think>` blocks (explicit reasoning markers) are dropped.
///
/// **Code-fence awareness:** while inside a ` ``` ` fenced code block,
/// `<final>` and `<think>` are treated as plain text rather than as state
/// tags. This lets a model show literal `<final>` example syntax inside a
/// tutorial code block without confusing the filter.
///
/// **Chunk-boundary handling:** partial tag prefixes (`<f`, `<fi`, `<fin`,
/// `<fina`, `<final`, `</`, `</f`, …, `<t`, `<th`, `<thi`, …) and partial
/// fence delimiters (1-2 trailing backticks) are buffered until the next
/// `push` so a tag straddling a chunk boundary is recognized correctly.
///
/// Use [`Self::ever_in_final`] after streaming completes to decide whether
/// the model ever produced a `<final>` tag at all — when it has not, the
/// caller should typically substitute an empty-answer placeholder so the
/// user is not left staring at a blank reply.
pub struct FinalTagFilter {
    /// Depth counter for nested `<final>` blocks. >0 means content is
    /// user-facing. Using a depth counter rather than a bool prevents a
    /// nested inner `</final>` from prematurely un-toggling the outer block.
    final_depth: u32,
    /// `true` once at least one `<final>` opening tag has been seen.
    ever_in_final: bool,
    /// Depth counter for nested `<think>` blocks, mirroring `final_depth`.
    think_depth: u32,
    /// Track open-fence state **only while in a `<final>` block** so that
    /// the model's pre-`<final>` "thinking out loud" monologue cannot leave
    /// the filter latched inside a code fence when the real `<final>` tag
    /// arrives. Outside `<final>` every byte is dropped anyway, so the
    /// fence state is meaningless there.
    in_code_fence: bool,
    pending: String,
}

impl FinalTagFilter {
    pub fn new() -> Self {
        Self {
            final_depth: 0,
            ever_in_final: false,
            think_depth: 0,
            in_code_fence: false,
            pending: String::new(),
        }
    }

    /// Push the next chunk of text. Returns the portion that should be
    /// emitted (may be empty when the chunk is entirely outside a `<final>`
    /// block, entirely inside a `<think>` block, or fully buffered as a
    /// partial tag).
    pub fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let text = std::mem::take(&mut self.pending);
        let (output, hold) = self.process_owned(text);
        self.pending = hold;
        output
    }

    /// Flush any remaining buffered content. Call exactly once after the
    /// stream ends. The buffer at this point is either a partial tag prefix
    /// (e.g., `<fi` that never resolved) or trailing partial backticks; both
    /// cases are emitted as plain text under the same emission rule.
    pub fn flush(&mut self) -> String {
        let text = std::mem::take(&mut self.pending);
        let mut output = String::new();
        self.emit_text(&text, &mut output);
        output
    }

    /// `true` once the stream has contained at least one `<final>` opening
    /// tag. The kernel uses this to decide whether the empty-answer
    /// placeholder should kick in.
    pub fn ever_in_final(&self) -> bool {
        self.ever_in_final
    }

    fn is_emitting(&self) -> bool {
        self.final_depth > 0 && self.think_depth == 0
    }

    fn emit_text(&self, s: &str, output: &mut String) {
        if self.is_emitting() {
            output.push_str(s);
        }
    }

    fn process_owned(&mut self, text: String) -> (String, String) {
        let bytes = text.as_bytes();
        let mut output = String::new();
        let mut cursor = 0;

        while cursor < bytes.len() {
            let next_special = bytes[cursor..]
                .iter()
                .position(|&b| b == b'<' || b == b'`')
                .map(|i| cursor + i);

            let pos = match next_special {
                Some(p) => p,
                None => {
                    self.emit_text(&text[cursor..], &mut output);
                    return (output, String::new());
                }
            };

            self.emit_text(&text[cursor..pos], &mut output);

            if bytes[pos] == b'`' {
                // Code-fence tracking is only load-bearing inside a <final>
                // block — outside, every byte is dropped so the fence state
                // is meaningless. Gating the toggle on final_depth prevents
                // an unclosed fence in the model's pre-<final> monologue
                // from latching the filter into code-fence mode and
                // swallowing the real <final> tag when it finally arrives.
                if self.final_depth == 0 {
                    self.emit_text(&text[pos..pos + 1], &mut output);
                    cursor = pos + 1;
                    continue;
                }

                if pos + 3 <= bytes.len() && bytes[pos + 1] == b'`' && bytes[pos + 2] == b'`' {
                    self.in_code_fence = !self.in_code_fence;
                    self.emit_text(&text[pos..pos + 3], &mut output);
                    cursor = pos + 3;
                    continue;
                }
                // Possibly partial fence at end of buffer — only hold when
                // inside <final>, so outside-<final> backticks can't pile up
                // unbounded pending state.
                let trailing = bytes.len() - pos;
                if trailing < 3 && bytes[pos..].iter().all(|&b| b == b'`') {
                    return (output, text[pos..].to_string());
                }
                // Lone backtick or backtick pair followed by non-backtick —
                // plain text.
                self.emit_text(&text[pos..pos + 1], &mut output);
                cursor = pos + 1;
                continue;
            }

            // bytes[pos] == b'<'
            if self.in_code_fence {
                // Inside a code fence inside <final>: `<final>` and `<think>`
                // are literal tutorial content, not state-toggling tags.
                self.emit_text(&text[pos..pos + 1], &mut output);
                cursor = pos + 1;
                continue;
            }

            match try_match_known_tag(bytes, pos) {
                TagMatch::Complete(kind, len) => {
                    match kind {
                        KnownTag::FinalOpen => {
                            self.final_depth = self.final_depth.saturating_add(1);
                            self.ever_in_final = true;
                        }
                        KnownTag::FinalClose => {
                            self.final_depth = self.final_depth.saturating_sub(1);
                        }
                        KnownTag::ThinkOpen => {
                            self.think_depth = self.think_depth.saturating_add(1);
                        }
                        KnownTag::ThinkClose => {
                            self.think_depth = self.think_depth.saturating_sub(1);
                        }
                    }
                    cursor = pos + len;
                }
                TagMatch::Partial => {
                    return (output, text[pos..].to_string());
                }
                TagMatch::None => {
                    self.emit_text(&text[pos..pos + 1], &mut output);
                    cursor = pos + 1;
                }
            }
        }

        (output, String::new())
    }
}

impl Default for FinalTagFilter {
    fn default() -> Self {
        Self::new()
    }
}

// ── ThinkTagFilter (streaming) ───────────────────────────────────────────────

/// Streaming filter that strips `<think>...</think>` blocks from chunks.
///
/// Simpler than [`FinalTagFilter`]: emits everything *except* content between
/// matched `<think>` and `</think>` tags. Depth-counted so nested think blocks
/// work. Code-fence aware so tutorial examples survive.
///
/// Used by [`ChatOutputFilter`] unconditionally (when `FinalTagFilter` is not
/// active) so the live SSE stream never leaks model reasoning to the user.
pub struct ThinkTagFilter {
    think_depth: u32,
    in_code_fence: bool,
    pending: String,
}

impl ThinkTagFilter {
    pub fn new() -> Self {
        Self {
            think_depth: 0,
            in_code_fence: false,
            pending: String::new(),
        }
    }

    pub fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let text = std::mem::take(&mut self.pending);
        let (output, hold) = self.process_owned(text);
        self.pending = hold;
        output
    }

    pub fn flush(&mut self) -> String {
        let text = std::mem::take(&mut self.pending);
        let mut output = String::new();
        if self.think_depth == 0 {
            output.push_str(&text);
        }
        output
    }

    fn is_emitting(&self) -> bool {
        self.think_depth == 0
    }

    fn process_owned(&mut self, text: String) -> (String, String) {
        let bytes = text.as_bytes();
        let mut output = String::new();
        let mut cursor = 0;

        while cursor < bytes.len() {
            let next = bytes[cursor..]
                .iter()
                .position(|&b| b == b'<' || b == b'`')
                .map(|i| cursor + i);

            let pos = match next {
                Some(p) => p,
                None => {
                    if self.is_emitting() {
                        output.push_str(&text[cursor..]);
                    }
                    return (output, String::new());
                }
            };

            if self.is_emitting() {
                output.push_str(&text[cursor..pos]);
            }

            if bytes[pos] == b'`' {
                if pos + 3 <= bytes.len() && bytes[pos + 1] == b'`' && bytes[pos + 2] == b'`' {
                    self.in_code_fence = !self.in_code_fence;
                    if self.is_emitting() {
                        output.push_str(&text[pos..pos + 3]);
                    }
                    cursor = pos + 3;
                    continue;
                }
                let trailing = bytes.len() - pos;
                if trailing < 3 && bytes[pos..].iter().all(|&b| b == b'`') {
                    return (output, text[pos..].to_string());
                }
                if self.is_emitting() {
                    output.push(bytes[pos] as char);
                }
                cursor = pos + 1;
                continue;
            }

            // bytes[pos] == b'<'
            if self.in_code_fence {
                if self.is_emitting() {
                    output.push('<');
                }
                cursor = pos + 1;
                continue;
            }

            match try_match_known_tag(bytes, pos) {
                TagMatch::Complete(KnownTag::ThinkOpen, len) => {
                    self.think_depth = self.think_depth.saturating_add(1);
                    cursor = pos + len;
                }
                TagMatch::Complete(KnownTag::ThinkClose, len) => {
                    self.think_depth = self.think_depth.saturating_sub(1);
                    cursor = pos + len;
                }
                TagMatch::Complete(_, len) => {
                    if self.is_emitting() {
                        output.push_str(&text[pos..pos + len]);
                    }
                    cursor = pos + len;
                }
                TagMatch::Partial => {
                    return (output, text[pos..].to_string());
                }
                TagMatch::None => {
                    if self.is_emitting() {
                        output.push('<');
                    }
                    cursor = pos + 1;
                }
            }
        }

        (output, String::new())
    }
}

impl Default for ThinkTagFilter {
    fn default() -> Self {
        Self::new()
    }
}

// ── ChatOutputFilter (composer) ─────────────────────────────────────────────

/// Composes the Phase 1 fenced-tool-block sanitizer with tag-level filters
/// into one streaming surface so chat call sites only deal with one
/// push/flush API regardless of which mitigations are on.
///
/// Order of application:
/// 1. [`OutputSanitizerStream`] strips fenced ```json tool intent blocks.
/// 2. When `enforce_final_tag = true`: [`FinalTagFilter`] drops everything
///    outside `<final>...</final>` and strips `<think>`.
/// 3. When `enforce_final_tag = false`: [`ThinkTagFilter`] strips only
///    `<think>` blocks so model reasoning never leaks to the live SSE stream.
///
/// JSON tool blocks are removed first (step 1) so their contents (which
/// contain literal `<` characters from JSON strings) cannot confuse the
/// tag-matching passes in steps 2/3.
pub struct ChatOutputFilter {
    sanitizer: OutputSanitizerStream,
    /// When `enforce_final_tag = true`, this is `Some` and handles both
    /// `<final>` gating and `<think>` stripping in one pass.
    final_filter: Option<FinalTagFilter>,
    /// When `enforce_final_tag = false`, this strips `<think>` blocks from
    /// the live stream so reasoning doesn't flash then vanish on persist.
    think_filter: Option<ThinkTagFilter>,
}

impl ChatOutputFilter {
    pub fn new(enforce_final_tag: bool) -> Self {
        Self {
            sanitizer: OutputSanitizerStream::new(),
            final_filter: enforce_final_tag.then(FinalTagFilter::new),
            think_filter: if enforce_final_tag {
                None
            } else {
                Some(ThinkTagFilter::new())
            },
        }
    }

    pub fn push(&mut self, chunk: &str) -> String {
        let after_sanitize = self.sanitizer.push(chunk);
        if after_sanitize.is_empty() {
            return after_sanitize;
        }
        if let Some(f) = self.final_filter.as_mut() {
            return f.push(&after_sanitize);
        }
        if let Some(f) = self.think_filter.as_mut() {
            return f.push(&after_sanitize);
        }
        after_sanitize
    }

    pub fn flush(&mut self) -> String {
        let mut out = self.sanitizer.flush();
        if let Some(f) = self.final_filter.as_mut() {
            let mut filtered = f.push(&out);
            filtered.push_str(&f.flush());
            out = filtered;
        } else if let Some(f) = self.think_filter.as_mut() {
            let mut filtered = f.push(&out);
            filtered.push_str(&f.flush());
            out = filtered;
        }
        out
    }

    pub fn suppressed_block_count(&self) -> usize {
        self.sanitizer.suppressed_count()
    }
}

// ── XML tool-tag stripper (Phase 3) ─────────────────────────────────────────

/// Tag names recognized by [`strip_xml_tool_tags`]. Order does not matter.
/// All names are compared case-insensitively.
const KNOWN_TOOL_TAG_NAMES: &[&str] = &[
    "tool_call",
    "tool_calls",
    "tool_result",
    "function_call",
    "function_calls",
    "invoke",
    "minimax:tool_call",
];

/// Strip XML-style tool-call tags (and their bodies) from a complete text.
///
/// Recognized tags:
/// - `<tool_call>...</tool_call>`
/// - `<tool_calls>...</tool_calls>`
/// - `<tool_result>...</tool_result>`
/// - `<function_call>...</function_call>`
/// - `<function_calls>...</function_calls>`
/// - `<invoke>...</invoke>` (also self-closing `<invoke ... />`)
/// - `<minimax:tool_call>...</minimax:tool_call>`
///
/// Matching is **case-insensitive** on the tag name and permits XML
/// attributes on the opening tag (e.g., `<invoke name="search" id="c1">`).
/// Nested same-name tags are handled via a depth counter. Unclosed tags
/// strip everything from the opening tag to end-of-input — a defensive
/// choice because the subsequent body is almost certainly more internal
/// scaffolding the model emitted before it could close the tag.
///
/// This function is a **visible-output filter**. It does not re-promote
/// stripped XML-formatted intents into `result.tool_calls`; the kernel's
/// structured tool-use channel is the source of truth for actual execution.
pub fn strip_xml_tool_tags(text: &str) -> String {
    if text.is_empty() || !text.contains('<') {
        return text.to_string();
    }

    let bytes = text.as_bytes();
    // Code-region awareness (Phase 4): compute the byte ranges inside
    // fenced code blocks once, then skip any `<` that falls inside one.
    // This lets tutorial agents legitimately show literal tool-call syntax
    // inside ```xml ... ``` examples without losing it to this stripper.
    let code_regions = find_fenced_code_regions(text);
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        let lt_pos = match bytes[cursor..].iter().position(|&b| b == b'<') {
            Some(p) => cursor + p,
            None => {
                output.push_str(&text[cursor..]);
                break;
            }
        };

        // Emit everything up to the `<`.
        output.push_str(&text[cursor..lt_pos]);

        if is_in_regions(lt_pos, &code_regions) {
            // The `<` is inside a fenced code block — emit as plain text
            // and advance one byte so tutorial examples survive.
            output.push('<');
            cursor = lt_pos + 1;
            continue;
        }

        // Try to identify the tag name at `<`. Ignore leading `/` so we
        // don't match a stray close tag (it's either orphaned, which we
        // emit as plain text, or the matching open will have been handled
        // and we never reach here).
        let after_lt = lt_pos + 1;
        if after_lt >= bytes.len() {
            output.push_str(&text[lt_pos..]);
            break;
        }
        if bytes[after_lt] == b'/' {
            // Orphan close tag. Emit the `<` and advance one byte; the next
            // iteration will either find another `<` or finish the string.
            output.push('<');
            cursor = lt_pos + 1;
            continue;
        }

        let tag_name = match read_tag_name(bytes, after_lt) {
            Some(name) => name,
            None => {
                output.push('<');
                cursor = lt_pos + 1;
                continue;
            }
        };

        if !is_known_tool_tag(&tag_name) {
            output.push('<');
            cursor = lt_pos + 1;
            continue;
        }

        // Find the end of the opening tag (the first unquoted `>`).
        let open_gt = match find_unquoted_gt(bytes, after_lt + tag_name.len()) {
            Some(p) => p,
            None => {
                // Unclosed opening tag — strip everything from `<` to EOF.
                break;
            }
        };

        // Self-closing `<tag .../>` — just skip past `>`.
        let opening_body = &text[after_lt + tag_name.len()..open_gt];
        if opening_body.trim_end().ends_with('/') {
            cursor = open_gt + 1;
            continue;
        }

        // Find the matching close tag, honoring nested open tags with the
        // same name.
        let close_end = match find_matching_close(bytes, open_gt + 1, &tag_name) {
            Some(end) => end,
            None => {
                // No matching close — strip from `<` to EOF.
                break;
            }
        };
        cursor = close_end;
    }

    output
}

fn read_tag_name(bytes: &[u8], start: usize) -> Option<String> {
    let mut end = start;
    while end < bytes.len() {
        let b = bytes[end];
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b':' {
            end += 1;
        } else {
            break;
        }
    }
    if end == start {
        None
    } else {
        // Safe: scanned bytes are all ASCII so slicing at these indices is
        // char-aligned.
        Some(std::str::from_utf8(&bytes[start..end]).ok()?.to_string())
    }
}

fn is_known_tool_tag(name: &str) -> bool {
    KNOWN_TOOL_TAG_NAMES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(name))
}

fn find_unquoted_gt(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    let mut quote: Option<u8> = None;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                quote = Some(b);
            }
            b'>' => return Some(i),
            b'<' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

// ── Standalone <think> stripping ─────────────────────────────────────────────

/// Strip `<think>...</think>` blocks from a complete text. Used by the
/// Delivery profile when `<final>` enforcement is off (the default) so
/// that models emitting reasoning markers (Claude, DeepSeek, etc.) don't
/// leak their chain-of-thought to the user.
///
/// Uses the same tag-matching logic as [`FinalTagFilter`] but operates in
/// one shot on a complete text without needing any `<final>` wrapper.
/// Code-fence aware: `<think>` inside fenced ` ``` ` blocks is treated as
/// plain text.
fn strip_think_tags(text: &str) -> String {
    if !text.contains("<think") {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut in_think = false;
    let mut think_depth: u32 = 0;
    let mut in_code_fence = false;

    while cursor < bytes.len() {
        let next = bytes[cursor..]
            .iter()
            .position(|&b| b == b'<' || b == b'`')
            .map(|i| cursor + i);

        let pos = match next {
            Some(p) => p,
            None => {
                if !in_think {
                    output.push_str(&text[cursor..]);
                }
                break;
            }
        };

        if !in_think {
            output.push_str(&text[cursor..pos]);
        }

        if bytes[pos] == b'`' {
            if pos + 3 <= bytes.len() && bytes[pos + 1] == b'`' && bytes[pos + 2] == b'`' {
                in_code_fence = !in_code_fence;
                if !in_think {
                    output.push_str(&text[pos..pos + 3]);
                }
                cursor = pos + 3;
                continue;
            }
            if !in_think {
                output.push(bytes[pos] as char);
            }
            cursor = pos + 1;
            continue;
        }

        // bytes[pos] == b'<'
        if in_code_fence {
            if !in_think {
                output.push('<');
            }
            cursor = pos + 1;
            continue;
        }

        match try_match_known_tag(bytes, pos) {
            TagMatch::Complete(KnownTag::ThinkOpen, len) => {
                think_depth = think_depth.saturating_add(1);
                in_think = true;
                cursor = pos + len;
            }
            TagMatch::Complete(KnownTag::ThinkClose, len) => {
                think_depth = think_depth.saturating_sub(1);
                if think_depth == 0 {
                    in_think = false;
                }
                cursor = pos + len;
            }
            TagMatch::Complete(_, len) => {
                // Non-think tags (e.g., <final>) — emit as plain text.
                if !in_think {
                    output.push_str(&text[pos..pos + len]);
                }
                cursor = pos + len;
            }
            TagMatch::Partial => {
                // Could be a partial <think tag at EOF — emit as-is.
                if !in_think {
                    output.push_str(&text[pos..]);
                }
                break;
            }
            TagMatch::None => {
                if !in_think {
                    output.push('<');
                }
                cursor = pos + 1;
            }
        }
    }

    output
}

// ── Error payload rewriting (Phase 5) ────────────────────────────────────────

/// Maximum text length eligible for error-payload rewriting. Longer texts
/// are assumed to be normal assistant prose that merely *mentions* an error
/// rather than *being* a leaked error payload.
const ERROR_REWRITE_MAX_LEN: usize = 4096;

/// If `text` looks like a raw provider error payload (JSON API error, HTTP
/// status line, Cloudflare HTML page, context-overflow message, or
/// rate-limit notice), return a clean user-facing replacement message.
/// Returns `None` when the text does not match any known error pattern,
/// meaning it is normal prose and should be left alone.
///
/// This is intended as the **last** sanitization pass on user-visible text,
/// after tool-block extraction, XML tag stripping, and optional `<final>`
/// enforcement. The raw text in the model's context window is unaffected —
/// the model should see the original error so it can reason about retrying.
pub fn rewrite_error_payload(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > ERROR_REWRITE_MAX_LEN {
        return None;
    }

    // JSON API error: {"error": {"type": "...", "message": "..."}} or {"error": "..."}
    if trimmed.starts_with('{') {
        if let Some(msg) = extract_json_error_message(trimmed) {
            return Some(format!("LLM request failed: {msg}. Please try again."));
        }
    }

    let lower = trimmed.to_ascii_lowercase();

    // Cloudflare / HTML error pages — must check before HTTP status line
    // because Cloudflare pages sometimes contain status codes too.
    if (lower.contains("<!doctype") || lower.contains("<html"))
        && (lower.contains("cloudflare")
            || lower.contains("access denied")
            || lower.contains("ray id"))
    {
        return Some(
            "LLM request failed: received an error page from the provider's CDN. \
             Please try again."
                .to_string(),
        );
    }

    // Context overflow — only rewrite short texts (< 512 chars) to avoid
    // mangling tutorials that merely discuss context length concepts.
    if trimmed.len() < 512
        && (lower.contains("context length exceeded")
            || lower.contains("prompt is too long")
            || lower.contains("maximum context length")
            || lower.contains("request_too_large")
            || lower.contains("request too large")
            || lower.contains("exceeds model context window"))
    {
        return Some(
            "Context too large for this model. Try starting a fresh session, \
             or switch to a model with a larger context window."
                .to_string(),
        );
    }

    // Rate limiting — requires structural error indicators, not just
    // mentioning the concept. The guard prevents false positives on prose
    // like "Error handling: when you get a rate limit, back off."
    if trimmed.len() < 512
        && (lower.contains("rate limit")
            || lower.contains("too many requests")
            || lower.contains(" 429 ")
            || lower.contains(" 429:")
            || lower.ends_with("429"))
    {
        let looks_like_error = starts_with_error_prefix(&lower)
            || lower.starts_with('{')
            || lower.contains("rate_limit");
        if looks_like_error {
            return Some(
                "LLM request was rate-limited. Please wait a moment and try again.".to_string(),
            );
        }
    }

    // HTTP status line: "Error 500: ...", "HTTP 502 ...", "HTTP/1.1 503 ..."
    if is_http_error_line(trimmed) {
        return Some("LLM request failed (server error). Please try again.".to_string());
    }

    None
}

fn extract_json_error_message(json_str: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json_str).ok()?;
    let error = v.get("error")?;
    // {"error": "string message"}
    if let Some(s) = error.as_str() {
        return Some(truncate_error_message(s));
    }
    // {"error": {"message": "string message", ...}}
    if let Some(obj) = error.as_object() {
        if let Some(msg) = obj.get("message").and_then(|m| m.as_str()) {
            return Some(truncate_error_message(msg));
        }
    }
    None
}

fn truncate_error_message(msg: &str) -> String {
    let trimmed = msg.trim();
    // Scrub potentially sensitive content before including in user-facing text.
    if looks_sensitive(trimmed) {
        return "a provider error occurred (details hidden for security)".to_string();
    }
    if trimmed.len() <= 200 {
        trimmed.to_string()
    } else {
        let mut boundary = 200;
        while boundary > 0 && !trimmed.is_char_boundary(boundary) {
            boundary -= 1;
        }
        format!("{}...", &trimmed[..boundary])
    }
}

/// Return `true` if the string likely contains sensitive info that should
/// not be shown to the user (API keys, internal URLs, auth tokens).
fn looks_sensitive(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    // Common API key prefixes from supported providers (Anthropic sk-,
    // OpenAI sk-, X.AI xai-, Groq gsk_, AWS AKIA, GitHub ghp_/gho_,
    // webhook secrets whsec_) and generic auth tokens.
    if lower.contains("sk-")
        || lower.contains("key-")
        || lower.contains("token-")
        || lower.contains("bearer ")
        || lower.contains("xai-")
        || lower.contains("gsk_")
        || lower.contains("ghp_")
        || lower.contains("gho_")
        || lower.contains("whsec_")
        || lower.contains("password")
    {
        return true;
    }
    // AWS access key IDs start with AKIA (uppercase, 20 chars).
    if s.contains("AKIA") {
        return true;
    }
    // Internal-looking URLs (hostnames with ports, internal TLDs)
    if lower.contains("://") && (lower.contains("internal") || lower.contains("localhost")) {
        return true;
    }
    false
}

/// Check if the lowercase text starts with an error-like prefix followed by
/// a structural separator (digit, colon, `{`), NOT by prose like "handling".
fn starts_with_error_prefix(lower: &str) -> bool {
    for prefix in &["error", "http"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let next = rest.chars().next().unwrap_or(' ');
            // "error:" / "error 4" / "error {" / "http " / "http/"
            if next == ':' || next == '/' || next == ' ' || next == '{' {
                // Check that the char after whitespace is a digit, `{`, or `"`
                let after_sep = rest.trim_start_matches([':', ' ', '/']);
                if after_sep
                    .starts_with(|c: char| c.is_ascii_digit() || c == '{' || c == '"' || c == '.')
                {
                    return true;
                }
            }
        }
    }
    false
}

fn is_http_error_line(text: &str) -> bool {
    if text.len() >= 512 {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    // Strip the prefix to reach the status code.
    let after_prefix = lower
        .strip_prefix("error ")
        .or_else(|| {
            lower.strip_prefix("http/").map(|rest| {
                // Skip version like "1.1 " — consume digits, dots, then spaces.
                rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit())
                    .trim_start()
            })
        })
        .or_else(|| lower.strip_prefix("http "));
    if let Some(rest) = after_prefix {
        // Next chars should be a 3-digit status code (4xx or 5xx).
        let code: String = rest.chars().take(3).collect();
        if code.len() == 3
            && code.chars().all(|c| c.is_ascii_digit())
            && (code.starts_with('4') || code.starts_with('5'))
        {
            return true;
        }
    }
    false
}

/// Return the byte ranges covering the bodies of fenced code blocks in
/// `text`. One range per matching opening/closing fence pair. An unclosed
/// fence extends to end-of-input.
///
/// The body range starts just after the opening fence's line (so language
/// tags like ` ```rust ` are not considered "inside code") and ends at the
/// byte position of the closing ` ``` `. Fence delimiter bytes are
/// themselves outside the returned ranges.
fn find_fenced_code_regions(text: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = text.as_bytes();
    let mut regions = Vec::new();
    let mut cursor = 0;

    while let Some(open_pos) = find_triple_backtick(text, cursor) {
        // Consume language tag + terminating newline.
        let body_start = match consume_lang_tag_complete(text, open_pos + 3) {
            Some(b) => b,
            None => {
                // Opening fence has no terminating newline (either
                // truncated or a bare ``` at end-of-input). Treat
                // everything after the three backticks as the region
                // body — content after a bare fence is effectively
                // inside code until the next fence or EOF.
                let body = open_pos + 3;
                if body <= bytes.len() {
                    regions.push(body..bytes.len());
                }
                break;
            }
        };
        let close_pos = find_triple_backtick(text, body_start);
        match close_pos {
            Some(p) => {
                regions.push(body_start..p);
                cursor = p + 3;
            }
            None => {
                regions.push(body_start..bytes.len());
                break;
            }
        }
    }

    regions
}

fn is_in_regions(pos: usize, regions: &[std::ops::Range<usize>]) -> bool {
    regions.iter().any(|r| r.contains(&pos))
}

/// Find the byte index immediately after the matching `</name>` close tag
/// for an opening tag with `name`, starting the search at `start`. Returns
/// `None` if no matching close is found. Honors nested same-name opens with
/// a depth counter.
fn find_matching_close(bytes: &[u8], start: usize, name: &str) -> Option<usize> {
    let mut i = start;
    let mut depth: usize = 1;

    while i < bytes.len() {
        let next_lt = bytes[i..].iter().position(|&b| b == b'<')?;
        i += next_lt;

        // Try close tag first.
        let after_lt = i + 1;
        if after_lt >= bytes.len() {
            return None;
        }
        if bytes[after_lt] == b'/' {
            let name_start = after_lt + 1;
            if let Some(candidate) = read_tag_name(bytes, name_start) {
                if candidate.eq_ignore_ascii_case(name) {
                    let mut j = name_start + candidate.len();
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b'>' {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                        i = j + 1;
                        continue;
                    }
                }
            }
            i += 1;
            continue;
        }

        // Try open tag (for same-name depth tracking).
        if let Some(candidate) = read_tag_name(bytes, after_lt) {
            if candidate.eq_ignore_ascii_case(name) {
                // A malformed nested same-name open tag (missing its `>`)
                // propagates `None` to the caller, which fail-closes by
                // stripping from the original open tag through EOF — the
                // same treatment as the top-level unclosed case. This is
                // intentional: emitting a half-formed nested open tag back
                // to the user would just re-tempt the next turn.
                let tag_end_search = find_unquoted_gt(bytes, after_lt + candidate.len())?;
                let opening_body = &bytes[after_lt + candidate.len()..tag_end_search];
                let is_self_closing = opening_body
                    .iter()
                    .rposition(|&b| !b.is_ascii_whitespace())
                    .is_some_and(|p| opening_body[p] == b'/');
                if !is_self_closing {
                    depth += 1;
                }
                i = tag_end_search + 1;
                continue;
            }
        }

        i += 1;
    }

    None
}

// ── Internals ───────────────────────────────────────────────────────────────

/// One of the four tag patterns the [`FinalTagFilter`] recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KnownTag {
    FinalOpen,
    FinalClose,
    ThinkOpen,
    ThinkClose,
}

/// Result of trying to match a recognized tag at a given position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagMatch {
    /// Complete recognized tag — `(kind, len)` where `len` is the byte length
    /// of the matched tag including angle brackets.
    Complete(KnownTag, usize),
    /// The bytes at this position are a prefix of one of the known tags but
    /// the tag is incomplete — caller should buffer and retry on next chunk.
    Partial,
    /// Not a recognized tag at all — caller should treat the leading `<` as
    /// plain text and advance by one byte.
    None,
}

/// Longest recognized tag length in bytes. Equal to `b"</final>".len()` and
/// `b"</think>".len()`. This is the load-bearing upper bound on the
/// partial-prefix check in [`try_match_known_tag`] — if a future tag is
/// added to `KNOWN` that is longer than this, the const assert below will
/// fail to compile.
const MAX_KNOWN_TAG_LEN: usize = 8;

/// Try to match `<final>`, `</final>`, `<think>`, or `</think>` at `pos`
/// (where `bytes[pos] == b'<'`). Returns whether the match is complete,
/// partial, or absent.
fn try_match_known_tag(bytes: &[u8], pos: usize) -> TagMatch {
    const KNOWN: &[(&[u8], KnownTag)] = &[
        (b"<final>", KnownTag::FinalOpen),
        (b"</final>", KnownTag::FinalClose),
        (b"<think>", KnownTag::ThinkOpen),
        (b"</think>", KnownTag::ThinkClose),
    ];
    const _: () = {
        let mut i = 0;
        while i < KNOWN.len() {
            assert!(
                KNOWN[i].0.len() <= MAX_KNOWN_TAG_LEN,
                "MAX_KNOWN_TAG_LEN must cover every entry in KNOWN"
            );
            i += 1;
        }
    };

    let available = bytes.len() - pos;

    for (tag, kind) in KNOWN {
        if available >= tag.len() && &bytes[pos..pos + tag.len()] == *tag {
            return TagMatch::Complete(*kind, tag.len());
        }
    }

    // No complete match. Could the available bytes be a prefix of a known
    // tag? Once we have MAX_KNOWN_TAG_LEN bytes and no entry matched above,
    // this `<` definitely cannot be a known tag — any prefix match would
    // have succeeded as a complete match.
    if available < MAX_KNOWN_TAG_LEN {
        let suffix = &bytes[pos..];
        for (tag, _) in KNOWN {
            if tag.starts_with(suffix) {
                return TagMatch::Partial;
            }
        }
    }

    TagMatch::None
}

/// Return the byte index of the next ``` sequence at or after `from`, or `None`.
/// All bytes scanned are ASCII so the returned index is always a char boundary.
fn find_triple_backtick(s: &str, from: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.len() < 3 || from >= bytes.len() {
        return None;
    }
    // `end` is one past the last valid start position for a 3-byte match —
    // we can safely index `i+2` whenever `i < end` because `end == len - 2`.
    let end = bytes.len() - 2;
    let mut i = from;
    while i < end {
        if bytes[i] == b'`' && bytes[i + 1] == b'`' && bytes[i + 2] == b'`' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Number of trailing backticks in `s`, capped at 2. Used by the streaming
/// filter to hold back partial fence delimiters that may complete in the next
/// chunk.
fn trailing_backtick_count(s: &str) -> usize {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut n = 0;
    while n < 2 && n < len && bytes[len - 1 - n] == b'`' {
        n += 1;
    }
    n
}

/// Given `s` starting somewhere with the opening ``` already consumed
/// (`after_open` points just past the third backtick), return the byte index
/// of the start of the fence body (i.e., just after the language tag and
/// terminating newline). Returns `None` if no terminating newline is present
/// yet — the caller should hold the buffer and retry on the next chunk.
fn consume_lang_tag_complete(s: &str, after_open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = after_open;
    while i < bytes.len() && is_lang_char(bytes[i]) {
        i += 1;
    }
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'\n' {
        return Some(i + 1);
    }
    if bytes[i] == b'\r' {
        if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            return Some(i + 2);
        }
        if i + 1 == bytes.len() {
            return None;
        }
        return Some(i + 1);
    }
    None
}

fn is_lang_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'+'
}

/// Try to parse `body` as one or more JSON tool intents.
///
/// Returns `Some(intents)` only when the body parses as a sequence of one or
/// more JSON values **and every parsed value matches the tool-intent shape**.
/// Mixed bodies (e.g., one tool intent followed by a tutorial JSON) return
/// `None` so the block is left in place — safer not to touch.
fn parse_intent_body(body: &str) -> Option<Vec<ExtractedToolIntent>> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let de = serde_json::Deserializer::from_str(trimmed);
    let mut intents = Vec::new();
    for value in de.into_iter::<Value>() {
        let v = value.ok()?;
        let intent = value_to_intent(v)?;
        intents.push(intent);
    }
    if intents.is_empty() {
        None
    } else {
        Some(intents)
    }
}

fn value_to_intent(v: Value) -> Option<ExtractedToolIntent> {
    let obj = v.as_object()?;
    let tool = obj.get("tool")?.as_str()?.to_string();
    let intent_type = obj.get("intent_type")?.as_str()?.to_string();
    let payload = obj.get("payload")?.clone();
    Some(ExtractedToolIntent {
        tool,
        intent_type,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn intent(tool: &str, intent_type: &str, payload: Value) -> ExtractedToolIntent {
        ExtractedToolIntent {
            tool: tool.to_string(),
            intent_type: intent_type.to_string(),
            payload,
        }
    }

    // ── extract_tool_intent_blocks ──────────────────────────────────────────

    #[test]
    fn extract_handles_no_fences() {
        let text = "Hello world. Nothing to see here.";
        let result = extract_tool_intent_blocks(text);
        assert_eq!(result.cleaned_text, text);
        assert!(result.extracted.is_empty());
    }

    #[test]
    fn extract_single_tool_intent_block() {
        let text = "Before.\n\n```json\n{\"tool\": \"echo\", \"intent_type\": \"execute\", \"payload\": {\"text\": \"hi\"}}\n```\n\nAfter.";
        let result = extract_tool_intent_blocks(text);
        assert_eq!(result.extracted.len(), 1);
        assert_eq!(
            result.extracted[0],
            intent("echo", "execute", json!({"text": "hi"}))
        );
        assert!(!result.cleaned_text.contains("```"));
        assert!(result.cleaned_text.contains("Before."));
        assert!(result.cleaned_text.contains("After."));
    }

    #[test]
    fn extract_multi_intent_block() {
        let text = "Demo:\n\n```json\n{\"tool\": \"a\", \"intent_type\": \"query\", \"payload\": {}}\n{\"tool\": \"b\", \"intent_type\": \"query\", \"payload\": {\"limit\": 5}}\n{\"tool\": \"c\", \"intent_type\": \"read\", \"payload\": {}}\n```\nDone.";
        let result = extract_tool_intent_blocks(text);
        assert_eq!(result.extracted.len(), 3);
        assert_eq!(result.extracted[0].tool, "a");
        assert_eq!(result.extracted[1].tool, "b");
        assert_eq!(result.extracted[1].payload, json!({"limit": 5}));
        assert_eq!(result.extracted[2].tool, "c");
        assert!(!result.cleaned_text.contains("```"));
        assert!(!result.cleaned_text.contains("\"tool\""));
    }

    #[test]
    fn extract_leaves_non_tool_json_alone() {
        let text = "Look:\n```json\n{\"foo\": 1, \"bar\": [2,3]}\n```\nDone.";
        let result = extract_tool_intent_blocks(text);
        assert!(result.extracted.is_empty());
        assert_eq!(result.cleaned_text, text);
    }

    #[test]
    fn extract_leaves_mixed_block_alone() {
        let text = "```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n{\"foo\": 1}\n```";
        let result = extract_tool_intent_blocks(text);
        assert!(
            result.extracted.is_empty(),
            "mixed block should be left untouched"
        );
        assert_eq!(result.cleaned_text, text);
    }

    #[test]
    fn extract_leaves_unclosed_fence_alone() {
        let text =
            "Talking...\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n";
        let result = extract_tool_intent_blocks(text);
        assert!(result.extracted.is_empty());
        assert_eq!(result.cleaned_text, text);
    }

    #[test]
    fn extract_handles_block_at_start_of_text() {
        let text =
            "```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\nAfter.";
        let result = extract_tool_intent_blocks(text);
        assert_eq!(result.extracted.len(), 1);
        assert!(result.cleaned_text.starts_with("After."));
    }

    #[test]
    fn extract_two_separate_blocks() {
        let text = "First:\n\n```json\n{\"tool\": \"a\", \"intent_type\": \"query\", \"payload\": {}}\n```\n\nMiddle.\n\n```json\n{\"tool\": \"b\", \"intent_type\": \"query\", \"payload\": {}}\n```\nEnd.";
        let result = extract_tool_intent_blocks(text);
        assert_eq!(result.extracted.len(), 2);
        assert_eq!(result.extracted[0].tool, "a");
        assert_eq!(result.extracted[1].tool, "b");
        assert!(result.cleaned_text.contains("First:"));
        assert!(result.cleaned_text.contains("Middle."));
        assert!(result.cleaned_text.contains("End."));
        assert!(!result.cleaned_text.contains("```"));
    }

    #[test]
    fn extract_handles_fence_without_language_tag() {
        let text = "```\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```";
        let result = extract_tool_intent_blocks(text);
        assert_eq!(result.extracted.len(), 1);
        assert!(!result.cleaned_text.contains("```"));
    }

    #[test]
    fn extract_handles_payload_with_nested_objects() {
        let text = "```json\n{\"tool\": \"q\", \"intent_type\": \"query\", \"payload\": {\"nested\": {\"a\": [1,2,{\"b\": null}]}}}\n```";
        let result = extract_tool_intent_blocks(text);
        assert_eq!(result.extracted.len(), 1);
        assert_eq!(
            result.extracted[0].payload,
            json!({"nested": {"a": [1, 2, {"b": null}]}})
        );
    }

    // ── OutputSanitizerStream ───────────────────────────────────────────────

    /// Helper that pushes the entire `text` in one chunk and flushes.
    fn stream_one_shot(text: &str) -> (String, usize) {
        let mut s = OutputSanitizerStream::new();
        let mut out = s.push(text);
        out.push_str(&s.flush());
        (out, s.suppressed_count())
    }

    /// Helper that pushes one byte at a time to exercise chunk-boundary handling.
    fn stream_byte_by_byte(text: &str) -> (String, usize) {
        let mut s = OutputSanitizerStream::new();
        let mut out = String::new();
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            let chunk = ch.encode_utf8(&mut buf);
            out.push_str(&s.push(chunk));
        }
        out.push_str(&s.flush());
        (out, s.suppressed_count())
    }

    #[test]
    fn stream_one_shot_no_fences() {
        let text = "Hello world.";
        let (out, count) = stream_one_shot(text);
        assert_eq!(out, text);
        assert_eq!(count, 0);
    }

    #[test]
    fn stream_one_shot_suppresses_tool_block() {
        let text = "Before.\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\nAfter.";
        let (out, count) = stream_one_shot(text);
        assert_eq!(count, 1);
        assert!(!out.contains("\"tool\""));
        assert!(out.contains("Before."));
        assert!(out.contains("After."));
    }

    #[test]
    fn stream_byte_by_byte_suppresses_tool_block() {
        let text = "Hi.\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {\"k\": 1}}\n```\nBye.";
        let (out, count) = stream_byte_by_byte(text);
        assert_eq!(count, 1);
        assert!(!out.contains("\"tool\""));
        assert!(out.contains("Hi."));
        assert!(out.contains("Bye."));
    }

    #[test]
    fn stream_byte_by_byte_keeps_non_tool_block() {
        let text = "```json\n{\"foo\": 1}\n```";
        let (out, count) = stream_byte_by_byte(text);
        assert_eq!(count, 0);
        assert_eq!(out, text);
    }

    #[test]
    fn stream_chunk_split_inside_opening_fence() {
        let mut s = OutputSanitizerStream::new();
        let a = s.push("Hello.\n``");
        let b =
            s.push("`json\n{\"tool\": \"x\", \"intent_type\": \"q\", \"payload\": {}}\n```\nDone.");
        let tail = s.flush();
        let combined = format!("{a}{b}{tail}");
        assert_eq!(s.suppressed_count(), 1);
        assert!(!combined.contains("\"tool\""));
        assert!(combined.contains("Hello."));
        assert!(combined.contains("Done."));
    }

    #[test]
    fn stream_chunk_split_inside_body() {
        let mut s = OutputSanitizerStream::new();
        let a = s.push("```json\n{\"tool\": \"x\", \"inten");
        let b = s.push("t_type\": \"query\", \"payload\": {}}\n```\nAfter.");
        let tail = s.flush();
        let combined = format!("{a}{b}{tail}");
        assert_eq!(s.suppressed_count(), 1);
        assert!(!combined.contains("\"tool\""));
        assert!(combined.contains("After."));
    }

    #[test]
    fn stream_chunk_split_inside_closing_fence() {
        let mut s = OutputSanitizerStream::new();
        let a = s.push("```json\n{\"tool\": \"x\", \"intent_type\": \"q\", \"payload\": {}}\n``");
        let b = s.push("`\nDone.");
        let tail = s.flush();
        let combined = format!("{a}{b}{tail}");
        assert_eq!(s.suppressed_count(), 1);
        assert!(!combined.contains("\"tool\""));
        assert!(combined.contains("Done."));
    }

    #[test]
    fn stream_unclosed_fence_flushes_as_is() {
        let mut s = OutputSanitizerStream::new();
        let a = s.push("Talking.\n```json\n{\"tool\": \"x\"");
        let tail = s.flush();
        let combined = format!("{a}{tail}");
        assert_eq!(s.suppressed_count(), 0);
        assert!(combined.contains("```json"));
        assert!(combined.contains("\"tool\""));
    }

    #[test]
    fn stream_handles_unicode_in_body() {
        let text = "前\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {\"q\": \"日本語\"}}\n```\n後";
        let (out, count) = stream_byte_by_byte(text);
        assert_eq!(count, 1);
        assert!(out.contains("前"));
        assert!(out.contains("後"));
        assert!(!out.contains("\"tool\""));
    }

    #[test]
    fn stream_handles_unicode_outside_fence() {
        let text = "前 emoji 🦀 後 — nothing fenced.";
        let (out, count) = stream_byte_by_byte(text);
        assert_eq!(count, 0);
        assert_eq!(out, text);
    }

    #[test]
    fn stream_multiple_blocks_split_across_many_chunks() {
        let chunks = [
            "Start.\n",
            "```json\n",
            "{\"tool\": \"a\", \"intent_type\": \"query\", \"payload\": {}}\n",
            "```\n",
            "Middle.\n",
            "```json\n",
            "{\"tool\": \"b\", \"intent_type\": \"read\", \"payload\": {}}\n",
            "```\n",
            "End.",
        ];
        let mut s = OutputSanitizerStream::new();
        let mut combined = String::new();
        for chunk in chunks {
            combined.push_str(&s.push(chunk));
        }
        combined.push_str(&s.flush());
        assert_eq!(s.suppressed_count(), 2);
        assert!(combined.contains("Start."));
        assert!(combined.contains("Middle."));
        assert!(combined.contains("End."));
        assert!(!combined.contains("\"tool\""));
    }

    #[test]
    fn stream_users_actual_leak_example() {
        // The exact pattern the user reported in the chat UI.
        let body = r#"```json
{"tool": "web-search", "intent_type": "query", "payload": {"query": "AI agent developments AgentOS multi-agent systems January 2025", "limit": 5}}
{"tool": "agent-list", "intent_type": "query", "payload": {"status": "online"}}
{"tool": "datetime", "intent_type": "query", "payload": {}}
```"#;
        let prefix = "I'll run a live demonstration across multiple capabilities. Watch me research, process data, delegate analysis, and store insights — all coordinated together.\n\n";
        let suffix = "\n\nthis was a response from the AI.";
        let text = format!("{prefix}{body}{suffix}");

        let extraction = extract_tool_intent_blocks(&text);
        assert_eq!(extraction.extracted.len(), 3);
        assert_eq!(extraction.extracted[0].tool, "web-search");
        assert_eq!(extraction.extracted[1].tool, "agent-list");
        assert_eq!(extraction.extracted[2].tool, "datetime");
        assert!(!extraction.cleaned_text.contains("```"));
        assert!(!extraction.cleaned_text.contains("\"tool\""));
        assert!(extraction.cleaned_text.contains("live demonstration"));
        assert!(extraction.cleaned_text.contains("response from the AI"));

        let (stream_out, count) = stream_byte_by_byte(&text);
        assert_eq!(count, 1);
        assert!(!stream_out.contains("```"));
        assert!(!stream_out.contains("\"tool\""));
        assert!(stream_out.contains("live demonstration"));
        assert!(stream_out.contains("response from the AI"));
    }

    #[test]
    fn stream_held_partial_then_completed_non_tool_block_emits_intact() {
        // Regression: a non-tool fenced block whose closing ``` arrives in the
        // next chunk should be emitted in full when re-scanned. Earlier
        // versions of the streaming filter could leave the trailing partial
        // backticks dangling. Verify that two-chunk delivery preserves the
        // entire block as visible text.
        let mut s = OutputSanitizerStream::new();
        let a = s.push("Look:\n```json\n{\"foo\": 1, \"bar\": 2}\n``");
        let b = s.push("`\nDone.");
        let tail = s.flush();
        let combined = format!("{a}{b}{tail}");
        assert_eq!(s.suppressed_count(), 0);
        assert!(
            combined.contains("\"foo\""),
            "non-tool block body should survive intact"
        );
        assert!(combined.contains("```json"));
        assert!(combined.matches("```").count() == 2);
        assert!(combined.contains("Done."));
    }

    #[test]
    fn extract_strips_text_when_native_tool_calls_already_present() {
        // Mirrors the kernel-side coexistence case: an LLM adapter returned
        // structured tool calls AND the model also wrote a stray ```json
        // tool-intent block in its visible text. The text should be cleaned
        // (defense in depth), but the test only covers the extractor — the
        // kernel wiring guarantees that promotion only happens when
        // `result.tool_calls` is empty.
        let text = "I'll call the tool.\n\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\n\nDone.";
        let result = extract_tool_intent_blocks(text);
        assert_eq!(result.extracted.len(), 1);
        assert!(!result.cleaned_text.contains("```"));
        assert!(!result.cleaned_text.contains("\"tool\""));
        assert!(result.cleaned_text.contains("I'll call the tool."));
        assert!(result.cleaned_text.contains("Done."));
    }

    // ── FinalTagFilter ──────────────────────────────────────────────────────

    fn final_one_shot(text: &str) -> (String, bool) {
        let mut f = FinalTagFilter::new();
        let mut out = f.push(text);
        out.push_str(&f.flush());
        (out, f.ever_in_final())
    }

    fn final_byte_by_byte(text: &str) -> (String, bool) {
        let mut f = FinalTagFilter::new();
        let mut out = String::new();
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            out.push_str(&f.push(ch.encode_utf8(&mut buf)));
        }
        out.push_str(&f.flush());
        (out, f.ever_in_final())
    }

    #[test]
    fn final_filter_drops_outside_text_in_strict_mode() {
        let (out, ever) = final_one_shot("thinking out loud <final>real answer</final> trailing");
        assert!(ever);
        assert_eq!(out, "real answer");
    }

    #[test]
    fn final_filter_emits_nothing_when_no_final_tag() {
        let (out, ever) = final_one_shot("just plain prose with no tags at all");
        assert!(!ever);
        assert_eq!(out, "");
    }

    #[test]
    fn final_filter_strips_think_blocks_inside_final() {
        let (out, _) = final_one_shot("<final>before<think>secret reasoning</think>after</final>");
        assert_eq!(out, "beforeafter");
    }

    #[test]
    fn final_filter_strips_think_blocks_outside_final_too() {
        let (out, _) = final_one_shot(
            "<think>private</think><final>visible</final><think>also private</think>",
        );
        assert_eq!(out, "visible");
    }

    #[test]
    fn final_filter_handles_chunk_split_inside_open_tag() {
        let mut f = FinalTagFilter::new();
        let a = f.push("noise <fi");
        let b = f.push("nal>answer</final> trailing");
        let tail = f.flush();
        let combined = format!("{a}{b}{tail}");
        assert_eq!(combined, "answer");
        assert!(f.ever_in_final());
    }

    #[test]
    fn final_filter_handles_chunk_split_inside_close_tag() {
        let mut f = FinalTagFilter::new();
        let a = f.push("<final>answer</fi");
        let b = f.push("nal>noise");
        let tail = f.flush();
        let combined = format!("{a}{b}{tail}");
        assert_eq!(combined, "answer");
    }

    #[test]
    fn final_filter_handles_chunk_split_inside_think_open_tag() {
        let mut f = FinalTagFilter::new();
        let a = f.push("<final>before<th");
        let b = f.push("ink>secret</think>after</final>");
        let tail = f.flush();
        let combined = format!("{a}{b}{tail}");
        assert_eq!(combined, "beforeafter");
    }

    #[test]
    fn final_filter_passes_unknown_tags_through_inside_final() {
        let (out, _) = final_one_shot("<final><user_data>foo</user_data></final>");
        assert_eq!(out, "<user_data>foo</user_data>");
    }

    #[test]
    fn final_filter_treats_inner_tags_as_text_inside_code_fence() {
        // Inside a fenced code block, `<final>` and `</final>` are plain
        // text — the filter must not toggle state on them.
        let text = "<final>Here is an example: ```html\n<final>example</final>\n```\nDone.</final>";
        let (out, _) = final_one_shot(text);
        assert!(out.contains("```html"));
        assert!(out.contains("<final>example</final>"));
        assert!(out.contains("Done."));
        assert!(!out.contains("<final>Here is"));
    }

    #[test]
    fn final_filter_byte_by_byte_unicode_inside_final() {
        let text = "<final>前 emoji 🦀 後</final>";
        let (out, ever) = final_byte_by_byte(text);
        assert!(ever);
        assert_eq!(out, "前 emoji 🦀 後");
    }

    #[test]
    fn final_filter_partial_tag_at_eof_emits_inside_final() {
        // Stream ends with a partial tag prefix that never resolved. Whatever
        // is in the buffer is emitted under the same emission rule (in_final
        // && !in_think). Inside <final>, partial gets shown.
        let mut f = FinalTagFilter::new();
        let a = f.push("<final>partial <fi");
        let tail = f.flush();
        let combined = format!("{a}{tail}");
        assert_eq!(combined, "partial <fi");
    }

    #[test]
    fn final_filter_partial_tag_at_eof_dropped_outside_final() {
        let mut f = FinalTagFilter::new();
        let a = f.push("noise <fi");
        let tail = f.flush();
        let combined = format!("{a}{tail}");
        assert_eq!(combined, "");
    }

    #[test]
    fn final_filter_lone_less_than_passed_through_inside_final() {
        let (out, _) = final_one_shot("<final>1 < 2 and 3 > 2</final>");
        assert_eq!(out, "1 < 2 and 3 > 2");
    }

    #[test]
    fn final_filter_unclosed_fence_outside_final_does_not_latch() {
        // Regression for the critical bug found in the Phase 2 review: an
        // unclosed ``` fence in the model's pre-<final> monologue used to
        // latch the filter into code-fence mode, which then passed the real
        // <final> opening tag through as plain text (suppressed) and left
        // ever_in_final == false. With the fix, fence tracking is gated on
        // final_depth > 0 so this scenario cannot occur.
        let text = "thinking out loud ```python\nprint('hi')\n<final>real answer</final>";
        let (out, ever) = final_one_shot(text);
        assert!(ever, "the real <final> tag must still be recognized");
        assert_eq!(out, "real answer");

        // And the byte-by-byte streamed version of the same input.
        let (stream_out, stream_ever) = final_byte_by_byte(text);
        assert!(stream_ever);
        assert_eq!(stream_out, "real answer");
    }

    #[test]
    fn final_filter_nested_think_blocks_use_depth_counter() {
        // A <think> nested inside another <think> must take two </think>
        // closes before content becomes visible again. A bool flag would
        // un-think on the first inner close and leak the outer reasoning.
        let (out, _) = final_one_shot(
            "<final>before<think>outer<think>inner</think>still-hidden</think>after</final>",
        );
        assert_eq!(out, "beforeafter");
    }

    #[test]
    fn final_filter_nested_final_blocks_use_depth_counter() {
        // Similarly, a nested <final> must take two closes before emission
        // stops. This is unlikely in practice but the depth counter gives
        // deterministic semantics rather than surprising off-by-one.
        let (out, _) = final_one_shot("<final>outer-start <final>inner</final> outer-end</final>");
        assert_eq!(out, "outer-start inner outer-end");
    }

    // ── ChatOutputFilter ────────────────────────────────────────────────────

    #[test]
    fn composite_strict_off_acts_like_phase_1_alone() {
        let mut composite = ChatOutputFilter::new(false);
        let mut sanitizer = OutputSanitizerStream::new();
        let text = "Hi.\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\nBye.";

        let mut composite_out = composite.push(text);
        composite_out.push_str(&composite.flush());

        let mut sanitizer_out = sanitizer.push(text);
        sanitizer_out.push_str(&sanitizer.flush());

        assert_eq!(composite_out, sanitizer_out);
        assert_eq!(composite.suppressed_block_count(), 1);
    }

    #[test]
    fn composite_strict_on_drops_text_outside_final() {
        let mut composite = ChatOutputFilter::new(true);
        let text = "thinking <final>visible answer</final> more thinking";
        let mut out = composite.push(text);
        out.push_str(&composite.flush());
        assert_eq!(out, "visible answer");
    }

    #[test]
    fn composite_strict_on_no_final_tag_returns_empty_string() {
        let mut composite = ChatOutputFilter::new(true);
        let text = "the model forgot to use the convention";
        let mut out = composite.push(text);
        out.push_str(&composite.flush());
        assert_eq!(out, "");
    }

    #[test]
    fn composite_strict_on_strips_tool_block_then_enforces_final() {
        // The model emits a fenced tool intent block AND wraps its answer in
        // <final>. Both layers should fire: tool block stripped, only the
        // <final> contents reach the user.
        let text = "thinking...\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\nstill thinking <final>here is the answer</final> trailing";
        let mut composite = ChatOutputFilter::new(true);
        let mut out = composite.push(text);
        out.push_str(&composite.flush());
        assert_eq!(out, "here is the answer");
        assert_eq!(composite.suppressed_block_count(), 1);
    }

    #[test]
    fn composite_byte_by_byte_strict_on() {
        let text = "noise <final>real <think>hidden</think>answer</final> noise";
        let mut composite = ChatOutputFilter::new(true);
        let mut out = String::new();
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            out.push_str(&composite.push(ch.encode_utf8(&mut buf)));
        }
        out.push_str(&composite.flush());
        assert_eq!(out, "real answer");
    }

    // ── strip_xml_tool_tags (Phase 3) ───────────────────────────────────────

    #[test]
    fn strip_xml_empty_and_no_tags() {
        assert_eq!(strip_xml_tool_tags(""), "");
        assert_eq!(strip_xml_tool_tags("plain prose"), "plain prose");
        assert_eq!(strip_xml_tool_tags("angle < bracket"), "angle < bracket");
    }

    #[test]
    fn strip_xml_single_tool_call_tag() {
        let text = "Before.<tool_call>{\"name\":\"x\"}</tool_call>After.";
        assert_eq!(strip_xml_tool_tags(text), "Before.After.");
    }

    #[test]
    fn strip_xml_tool_result_tag() {
        let text = "Result: <tool_result>{\"ok\": true}</tool_result> done";
        assert_eq!(strip_xml_tool_tags(text), "Result:  done");
    }

    #[test]
    fn strip_xml_function_call_and_calls() {
        let a = "pre<function_call>body</function_call>post";
        let b = "pre<function_calls>body</function_calls>post";
        assert_eq!(strip_xml_tool_tags(a), "prepost");
        assert_eq!(strip_xml_tool_tags(b), "prepost");
    }

    #[test]
    fn strip_xml_invoke_tag() {
        let text = "a<invoke name=\"search\"><parameter name=\"q\">hi</parameter></invoke>b";
        assert_eq!(strip_xml_tool_tags(text), "ab");
    }

    #[test]
    fn strip_xml_minimax_tool_call() {
        let text = "x<minimax:tool_call><invoke name=\"y\">z</invoke></minimax:tool_call>w";
        assert_eq!(strip_xml_tool_tags(text), "xw");
    }

    #[test]
    fn strip_xml_case_insensitive_open_and_close() {
        // Underscores are significant (the known-name list is snake_case),
        // but letter case is not. `<TOOL_CALL>` and `</Tool_Call>` both
        // match the `tool_call` entry.
        let text = "a<TOOL_CALL>body</Tool_Call>b";
        assert_eq!(strip_xml_tool_tags(text), "ab");
    }

    #[test]
    fn strip_xml_case_insensitive_minimax_namespace() {
        // The `minimax:` namespace prefix is also case-folded, so any
        // capitalization variant a model produces is caught.
        let text = "x<MiniMax:Tool_Call>y</minimax:TOOL_CALL>z";
        assert_eq!(strip_xml_tool_tags(text), "xz");
    }

    #[test]
    fn strip_xml_attributes_on_opening_tag() {
        let text = "a<tool_call id=\"call_1\" lang=\"en\">body</tool_call>b";
        assert_eq!(strip_xml_tool_tags(text), "ab");
    }

    #[test]
    fn strip_xml_attribute_with_gt_inside_quotes() {
        let text = "a<tool_call expr=\"x > 0\">body</tool_call>b";
        assert_eq!(strip_xml_tool_tags(text), "ab");
    }

    #[test]
    fn strip_xml_unclosed_tag_strips_to_eof() {
        let text = "before<tool_call>unterminated body with more text";
        assert_eq!(strip_xml_tool_tags(text), "before");
    }

    #[test]
    fn strip_xml_nested_same_name_tag() {
        let text = "A<tool_call>outer<tool_call>inner</tool_call>rest-outer</tool_call>B";
        assert_eq!(strip_xml_tool_tags(text), "AB");
    }

    #[test]
    fn strip_xml_multiple_separate_blocks() {
        let text = "start <tool_call>one</tool_call> middle <tool_call>two</tool_call> end";
        assert_eq!(strip_xml_tool_tags(text), "start  middle  end");
    }

    #[test]
    fn strip_xml_leaves_unrelated_tags_alone() {
        let text = "<em>foo</em><final>bar</final>";
        assert_eq!(strip_xml_tool_tags(text), text);
    }

    #[test]
    fn strip_xml_self_closing_invoke() {
        let text = "a<invoke name=\"ping\"/>b";
        assert_eq!(strip_xml_tool_tags(text), "ab");
    }

    #[test]
    fn strip_xml_orphan_close_tag_passes_through() {
        let text = "no open here </tool_call> still fine";
        assert_eq!(strip_xml_tool_tags(text), text);
    }

    #[test]
    fn strip_xml_preserves_surrounding_prose() {
        let text = "Let me search for that.\n<tool_call>\n{\"name\": \"web-search\", \"arguments\": {\"query\": \"rust\"}}\n</tool_call>\nDone.";
        let out = strip_xml_tool_tags(text);
        assert!(out.contains("Let me search for that."));
        assert!(out.contains("Done."));
        assert!(!out.contains("<tool_call>"));
        assert!(!out.contains("web-search"));
    }

    #[test]
    fn strip_xml_unicode_in_body() {
        let text = "前<tool_call>日本語 body 🦀</tool_call>後";
        assert_eq!(strip_xml_tool_tags(text), "前後");
    }

    #[test]
    fn strip_xml_lone_less_than_preserved() {
        let text = "1 < 2 and 3 < 4";
        assert_eq!(strip_xml_tool_tags(text), text);
    }

    // ── find_fenced_code_regions (Phase 4) ──────────────────────────────────

    #[test]
    fn find_fenced_regions_no_fences() {
        assert!(find_fenced_code_regions("no fences here").is_empty());
        assert!(find_fenced_code_regions("").is_empty());
    }

    #[test]
    fn find_fenced_regions_single_closed_block() {
        let text = "pre\n```\nbody\n```\npost";
        let regions = find_fenced_code_regions(text);
        assert_eq!(regions.len(), 1);
        let body = &text[regions[0].clone()];
        assert!(body.contains("body"));
        assert!(!body.contains("```"));
    }

    #[test]
    fn find_fenced_regions_with_language_tag_excludes_tag() {
        let text = "```rust\nfn main() {}\n```";
        let regions = find_fenced_code_regions(text);
        assert_eq!(regions.len(), 1);
        let body = &text[regions[0].clone()];
        assert!(!body.contains("rust"));
        assert!(body.contains("fn main"));
    }

    #[test]
    fn find_fenced_regions_unclosed_block_extends_to_eof() {
        let text = "pre\n```\nbody forever";
        let regions = find_fenced_code_regions(text);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].end, text.len());
    }

    #[test]
    fn find_fenced_regions_multiple_non_overlapping_blocks() {
        let text = "a\n```\none\n```\nb\n```\ntwo\n```\nc";
        let regions = find_fenced_code_regions(text);
        assert_eq!(regions.len(), 2);
        let one = &text[regions[0].clone()];
        let two = &text[regions[1].clone()];
        assert!(one.contains("one"));
        assert!(two.contains("two"));
        assert!(regions[0].end < regions[1].start);
    }

    #[test]
    fn is_in_regions_helper_boundary_cases() {
        let regions = vec![5..10, 20..25];
        assert!(!is_in_regions(4, &regions));
        assert!(is_in_regions(5, &regions));
        assert!(is_in_regions(9, &regions));
        assert!(!is_in_regions(10, &regions)); // Range is half-open
        assert!(is_in_regions(22, &regions));
        assert!(!is_in_regions(30, &regions));
    }

    // ── strip_xml_tool_tags code-region awareness ───────────────────────────

    #[test]
    fn strip_xml_preserves_tool_call_inside_fenced_block() {
        // Tutorial example: the <tool_call> tag inside a ```xml block should
        // survive unchanged so docs agents can legitimately show the syntax.
        let text =
            "Example:\n```xml\n<tool_call>\n{\"name\": \"search\"}\n</tool_call>\n```\nGot it?";
        let out = strip_xml_tool_tags(text);
        assert_eq!(out, text);
    }

    #[test]
    fn strip_xml_strips_leak_outside_fence_even_when_example_block_present() {
        // A tutorial shows an example in a fenced block AND the model also
        // emits a real <tool_call> leak outside any fence. Only the leak
        // should be stripped.
        let text = "Here's the syntax:\n```xml\n<tool_call>example</tool_call>\n```\nNow I'll call it: <tool_call>real leak</tool_call> done.";
        let out = strip_xml_tool_tags(text);
        assert!(out.contains("```xml"));
        assert!(out.contains("<tool_call>example</tool_call>"));
        assert!(!out.contains("real leak"));
        assert!(out.contains("Now I'll call it:"));
        assert!(out.contains("done."));
    }

    #[test]
    fn strip_xml_preserves_minimax_namespace_inside_fence() {
        let text = "See: ```\n<minimax:tool_call>body</minimax:tool_call>\n```";
        assert_eq!(strip_xml_tool_tags(text), text);
    }

    #[test]
    fn strip_xml_unclosed_fence_protects_rest_of_text() {
        // Regression: an unclosed ``` fence before a <tool_call> tag means
        // the tag lives inside the (implicit-to-EOF) code region, so it
        // should be preserved.
        let text =
            "Look ```\npretend this is all code\n<tool_call>safe</tool_call>\nstill pretend code";
        let out = strip_xml_tool_tags(text);
        assert_eq!(out, text);
    }

    #[test]
    fn strip_xml_leak_between_two_closed_fences_is_stripped() {
        let text = "```\nexample1\n```\n<tool_call>leak</tool_call>\n```\nexample2\n```";
        let out = strip_xml_tool_tags(text);
        assert!(!out.contains("<tool_call>"));
        assert!(!out.contains("leak"));
        assert!(out.contains("example1"));
        assert!(out.contains("example2"));
    }

    #[test]
    fn strip_xml_inline_backticks_do_not_create_region() {
        // Single and double backticks are not fenced code — they do not
        // create protected regions. A leaked <tool_call> between lone
        // backticks is still stripped.
        let text = "text `inline` <tool_call>leak</tool_call> more ``double`` end";
        let out = strip_xml_tool_tags(text);
        assert!(!out.contains("leak"));
        assert!(out.contains("`inline`"));
        assert!(out.contains("``double``"));
    }

    // ── rewrite_error_payload (Phase 5) ─────────────────────────────────────

    #[test]
    fn error_rewrite_normal_prose_returns_none() {
        assert!(rewrite_error_payload("The weather in Tokyo is 18°C.").is_none());
        assert!(rewrite_error_payload("").is_none());
        assert!(rewrite_error_payload("   ").is_none());
    }

    #[test]
    fn error_rewrite_json_error_with_message_object() {
        let text = r#"{"error": {"type": "invalid_request_error", "message": "prompt is too long for this model"}}"#;
        let out = rewrite_error_payload(text).unwrap();
        assert!(out.contains("prompt is too long for this model"));
        assert!(out.contains("Please try again"));
    }

    #[test]
    fn error_rewrite_json_error_with_string_message() {
        let text = r#"{"error": "internal server error"}"#;
        let out = rewrite_error_payload(text).unwrap();
        assert!(out.contains("internal server error"));
        assert!(out.contains("LLM request failed"));
    }

    #[test]
    fn error_rewrite_json_non_error_object_returns_none() {
        // A JSON object that is NOT an error payload should be left alone.
        let text = r#"{"result": "success", "data": [1, 2, 3]}"#;
        assert!(rewrite_error_payload(text).is_none());
    }

    #[test]
    fn error_rewrite_cloudflare_html_page() {
        let text = "<!DOCTYPE html><html><head><title>Access denied | example.com used Cloudflare</title></head><body>Ray ID: abc123</body></html>";
        let out = rewrite_error_payload(text).unwrap();
        assert!(out.contains("error page"));
        assert!(out.contains("CDN"));
    }

    #[test]
    fn error_rewrite_context_overflow() {
        for phrase in &[
            "context length exceeded",
            "This model's maximum context length is 128000 tokens. However, your messages resulted in 200000 tokens.",
            "prompt is too long",
            "request_too_large: the input was too long",
            "Request too large for model gpt-4",
        ] {
            let out = rewrite_error_payload(phrase);
            assert!(
                out.is_some(),
                "Should detect context overflow in: {phrase}"
            );
            let msg = out.unwrap();
            assert!(msg.contains("Context too large"), "msg: {msg}");
        }
    }

    #[test]
    fn error_rewrite_rate_limit() {
        let text = "Error 429: rate limit exceeded for this model";
        let out = rewrite_error_payload(text).unwrap();
        assert!(out.contains("rate-limited"));
    }

    #[test]
    fn error_rewrite_http_status_line() {
        let text = "Error 500: Internal Server Error";
        let out = rewrite_error_payload(text).unwrap();
        assert!(out.contains("server error"));
    }

    #[test]
    fn error_rewrite_http_with_version() {
        let text = "HTTP/1.1 502 Bad Gateway";
        let out = rewrite_error_payload(text).unwrap();
        assert!(out.contains("server error"));
    }

    #[test]
    fn error_rewrite_long_normal_text_not_rewritten() {
        // A long essay that happens to mention "error" should not be
        // rewritten (exceeds ERROR_REWRITE_MAX_LEN or doesn't match
        // structural patterns).
        let text = format!(
            "{}{}",
            "This is a long essay about software engineering. ".repeat(100),
            "Sometimes we encounter an error 500 in production."
        );
        assert!(rewrite_error_payload(&text).is_none());
    }

    #[test]
    fn error_rewrite_json_error_long_message_truncated() {
        let long_msg = "x".repeat(500);
        let text = format!(r#"{{"error": {{"message": "{}"}}}}"#, long_msg);
        let out = rewrite_error_payload(&text).unwrap();
        assert!(out.contains("..."));
        assert!(out.len() < 300);
    }

    #[test]
    fn error_rewrite_rate_limit_in_normal_prose_not_rewritten() {
        // A normal response that discusses rate limiting as a concept
        // should NOT be rewritten — the text is long enough and doesn't
        // start with an error prefix.
        let text = "To avoid hitting the rate limit, you should implement exponential backoff. When you get a 429 response, wait and retry.";
        assert!(rewrite_error_payload(text).is_none());
    }

    #[test]
    fn error_rewrite_error_handling_tutorial_not_rewritten() {
        // Regression for review finding #1: short prose starting with
        // "Error" that discusses rate limits conceptually must NOT be
        // rewritten. The guard now requires a structural separator (digit,
        // colon, `{`) after "error", not just the word itself.
        let text = "Error handling: when you get a rate limit, back off and retry.";
        assert!(
            rewrite_error_payload(text).is_none(),
            "tutorial about error handling should not be rewritten"
        );
    }

    #[test]
    fn error_rewrite_json_error_with_api_key_scrubs_sensitive_info() {
        // Review finding #2: provider error messages that contain API key
        // prefixes should be scrubbed rather than echoed to the user.
        let text = r#"{"error": {"type": "authentication_error", "message": "Invalid API key: sk-abc1234567890..."}}"#;
        let out = rewrite_error_payload(text).unwrap();
        assert!(!out.contains("sk-"), "API key prefix must not leak to user");
        assert!(out.contains("provider error occurred"));
    }

    #[test]
    fn error_rewrite_json_error_with_internal_url_scrubs() {
        let text = r#"{"error": {"message": "Connection refused: http://internal-llm.example.internal:8080/v1/chat"}}"#;
        let out = rewrite_error_payload(text).unwrap();
        assert!(
            !out.contains("internal"),
            "internal URL must not leak to user"
        );
        assert!(out.contains("provider error occurred"));
    }

    // ── sanitize_visible_text profiles (Phase 6) ────────────────────────────

    #[test]
    fn profile_delivery_strips_tool_leaks() {
        let text = "thinking\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\nLet me call it.\n<tool_call>leak</tool_call>\nDone.";
        let r = sanitize_visible_text(text, SanitizeProfile::Delivery, false);
        // Tool block extracted
        assert_eq!(r.extracted_intents.len(), 1);
        // Fenced block gone
        assert!(!r.text.contains("```"));
        // XML tag stripped
        assert!(!r.text.contains("<tool_call>"));
        // Prose preserved
        assert!(r.text.contains("thinking"));
        assert!(r.text.contains("Done."));
    }

    #[test]
    fn profile_delivery_rewrites_standalone_error() {
        // Error rewriter fires when the entire text is an error payload and
        // wraps the extracted message in a clean format.
        let text = r#"{"error": {"message": "bad request"}}"#;
        let r = sanitize_visible_text(text, SanitizeProfile::Delivery, false);
        assert!(r.text.starts_with("LLM request failed"));
        assert!(
            !r.text.contains(r#""error""#),
            "raw JSON key must not survive"
        );
    }

    #[test]
    fn profile_delivery_with_final_enforcement() {
        let text = "noise <final>answer</final> more noise";
        let r = sanitize_visible_text(text, SanitizeProfile::Delivery, true);
        assert_eq!(r.text, "answer");
    }

    #[test]
    fn profile_history_strips_tool_leaks_but_preserves_errors() {
        let text = "thinking\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\nLet me reason.\n<tool_call>leak</tool_call>\n{\"error\": \"bad request\"}";
        let r = sanitize_visible_text(text, SanitizeProfile::History, false);
        // Tool block extracted
        assert_eq!(r.extracted_intents.len(), 1);
        // Fenced block gone
        assert!(!r.text.contains("```"));
        // XML tag stripped
        assert!(!r.text.contains("<tool_call>"));
        // Error preserved (model should see it for retry reasoning)
        assert!(r.text.contains("bad request"));
        // Reasoning prose preserved
        assert!(r.text.contains("Let me reason."));
    }

    #[test]
    fn profile_history_ignores_final_enforcement() {
        // Even when enforce_final_tag is true, History profile does NOT
        // filter by <final> — it preserves reasoning for the context window.
        let text = "noise <final>answer</final> more noise";
        let r = sanitize_visible_text(text, SanitizeProfile::History, true);
        assert!(r.text.contains("noise"));
        assert!(r.text.contains("answer"));
        assert!(r.text.contains("more noise"));
    }

    #[test]
    fn profile_debug_only_strips_fenced_blocks() {
        let text = "thinking\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\nLet me reason.\n<tool_call>leak</tool_call>\n{\"error\": \"bad\"}";
        let r = sanitize_visible_text(text, SanitizeProfile::Debug, false);
        // Tool block extracted
        assert_eq!(r.extracted_intents.len(), 1);
        // Fenced block gone
        assert!(!r.text.contains("```"));
        // XML tags preserved (debug shows raw)
        assert!(r.text.contains("<tool_call>"));
        // Errors preserved
        assert!(r.text.contains("bad"));
    }

    // ── Standalone <think> stripping ───────────────────────────────────────

    #[test]
    fn delivery_strips_think_even_without_final_enforcement() {
        let text = "Before <think>reasoning here</think> after.";
        let r = sanitize_visible_text(text, SanitizeProfile::Delivery, false);
        assert!(!r.text.contains("<think>"));
        assert!(!r.text.contains("reasoning here"));
        assert!(r.text.contains("Before"));
        assert!(r.text.contains("after."));
    }

    #[test]
    fn delivery_strips_nested_think_blocks() {
        let text = "a<think>outer<think>inner</think>still-hidden</think>b";
        let r = sanitize_visible_text(text, SanitizeProfile::Delivery, false);
        assert_eq!(r.text, "ab");
    }

    #[test]
    fn delivery_think_stripping_preserves_think_inside_code_fence() {
        let text = "See: ```xml\n<think>example</think>\n```\nDone.";
        let r = sanitize_visible_text(text, SanitizeProfile::Delivery, false);
        assert!(r.text.contains("<think>example</think>"));
        assert!(r.text.contains("Done."));
    }

    #[test]
    fn history_does_not_strip_think_blocks() {
        let text = "Before <think>reasoning</think> after.";
        let r = sanitize_visible_text(text, SanitizeProfile::History, false);
        assert!(
            r.text.contains("<think>"),
            "History preserves think blocks for model context"
        );
    }

    #[test]
    fn profile_delivery_and_history_extract_same_intents() {
        let text = "```json\n{\"tool\": \"a\", \"intent_type\": \"query\", \"payload\": {}}\n```";
        let delivery = sanitize_visible_text(text, SanitizeProfile::Delivery, false);
        let history = sanitize_visible_text(text, SanitizeProfile::History, false);
        let debug = sanitize_visible_text(text, SanitizeProfile::Debug, false);
        assert_eq!(delivery.extracted_intents, history.extracted_intents);
        assert_eq!(delivery.extracted_intents, debug.extracted_intents);
        assert_eq!(delivery.extracted_intents.len(), 1);
    }

    // ── Full cross-phase integration (Phases 1-6 combined) ──────────────────

    #[test]
    fn integration_all_phases_combined_with_final_enforcement() {
        // A worst-case input that exercises every sanitization pass in one
        // shot: fenced JSON tool intent (Phase 1), XML tool tag (Phase 3),
        // <think> block (Phase 2), <final> wrapper (Phase 2), and an error
        // payload that would fire only if it were the entire text (Phase 5).
        // With enforce_final_tag=true, only <final> content survives.
        let input = concat!(
            "<think>Let me think about this carefully.</think>\n",
            "```json\n",
            "{\"tool\": \"web-search\", \"intent_type\": \"query\", \"payload\": {\"q\": \"rust\"}}\n",
            "```\n",
            "<tool_call>{\"name\": \"echo\"}</tool_call>\n",
            "<final>The answer is 42.</final>\n",
            "Some trailing reasoning the user should not see."
        );

        // Delivery + enforce_final_tag = strictest filtering.
        let delivery = sanitize_visible_text(input, SanitizeProfile::Delivery, true);
        assert_eq!(delivery.text, "The answer is 42.");
        assert_eq!(delivery.extracted_intents.len(), 1);
        assert_eq!(delivery.extracted_intents[0].tool, "web-search");

        // Delivery WITHOUT final enforcement — tool leaks + think stripped,
        // but all other prose preserved.
        let delivery_no_final = sanitize_visible_text(input, SanitizeProfile::Delivery, false);
        assert!(delivery_no_final.text.contains("The answer is 42."));
        assert!(delivery_no_final.text.contains("trailing reasoning"));
        assert!(!delivery_no_final.text.contains("```json"));
        assert!(!delivery_no_final.text.contains("<tool_call>"));
        // <think> blocks ARE stripped by Delivery even without <final>
        // enforcement, so model reasoning doesn't leak to the user.
        assert!(
            !delivery_no_final.text.contains("<think>"),
            "<think> must be stripped in Delivery"
        );
        assert!(!delivery_no_final.text.contains("Let me think"));
        assert_eq!(delivery_no_final.extracted_intents.len(), 1);

        // History — strips tool leaks but preserves reasoning and errors.
        let history = sanitize_visible_text(input, SanitizeProfile::History, false);
        assert!(!history.text.contains("```json"));
        assert!(!history.text.contains("<tool_call>"));
        assert!(history.text.contains("trailing reasoning"));
        assert!(history.text.contains("<think>"));
        assert_eq!(history.extracted_intents.len(), 1);

        // Debug — only strips fenced blocks.
        let debug = sanitize_visible_text(input, SanitizeProfile::Debug, false);
        assert!(!debug.text.contains("```json"));
        assert!(debug.text.contains("<tool_call>"));
        assert!(debug.text.contains("<think>"));
        assert_eq!(debug.extracted_intents.len(), 1);
    }

    #[test]
    fn integration_standalone_error_rewritten_only_in_delivery() {
        let input =
            r#"{"error": {"type": "invalid_request_error", "message": "prompt is too long"}}"#;

        let delivery = sanitize_visible_text(input, SanitizeProfile::Delivery, false);
        assert!(
            delivery.text.starts_with("LLM request failed"),
            "Delivery should rewrite error: {}",
            delivery.text
        );

        let history = sanitize_visible_text(input, SanitizeProfile::History, false);
        assert!(
            history.text.contains("prompt is too long"),
            "History should preserve raw error for model reasoning"
        );

        let debug = sanitize_visible_text(input, SanitizeProfile::Debug, false);
        assert!(
            debug.text.contains("invalid_request_error"),
            "Debug should preserve full raw error"
        );
    }

    // ── Streaming vs complete-text consistency ──────────────────────────────

    #[test]
    fn streaming_and_complete_text_agree_on_visible_vs_hidden() {
        // The live SSE stream is filtered by ChatOutputFilter (chunked)
        // while the persisted answer comes from sanitize_visible_text on
        // the complete text. Both paths must agree on which content is
        // VISIBLE vs HIDDEN. Whitespace may differ: the complete-text
        // extractor trims surrounding newlines around removed blocks for
        // cosmetic cleanup, while the streaming filter can't (chunks are
        // already emitted). The persisted answer (complete-text version)
        // is authoritative for display on page refresh.
        let cases: &[(bool, &str, &[&str], &[&str])] = &[
            // (enforce_final, input, must_contain, must_not_contain)
            (
                false,
                "Hi.\n```json\n{\"tool\": \"x\", \"intent_type\": \"query\", \"payload\": {}}\n```\nBye.",
                &["Hi.", "Bye."],
                &["```", "\"tool\""],
            ),
            (
                true,
                "noise <final>answer</final> trailing",
                &["answer"],
                &["noise", "trailing"],
            ),
            (
                true,
                "<think>hidden</think> <final>visible</final> dropped",
                &["visible"],
                &["hidden", "dropped"],
            ),
            (
                false,
                "prose with no special content",
                &["prose with no special content"],
                &[],
            ),
            // Key case: <think> stripping with enforce_final=false. Both the
            // streaming path (ThinkTagFilter) and the complete-text path
            // (strip_think_tags via Delivery profile) must agree on hiding
            // the reasoning content.
            (
                false,
                "before <think>secret reasoning</think> after",
                &["before", "after"],
                &["secret reasoning", "<think>"],
            ),
        ];

        for &(enforce_final, input, must_contain, must_not_contain) in cases {
            let mut filter = ChatOutputFilter::new(enforce_final);
            let mut streamed = filter.push(input);
            streamed.push_str(&filter.flush());

            // Use sanitize_visible_text(Delivery) for the complete-text path
            // so the comparison faithfully represents what the kernel
            // persists. This includes fenced extraction, XML stripping,
            // <final>/<think> filtering, and error rewriting.
            let complete_result =
                sanitize_visible_text(input, SanitizeProfile::Delivery, enforce_final);
            let complete = complete_result.text;

            for expected in must_contain {
                assert!(
                    streamed.contains(expected),
                    "streaming missing '{expected}' for enforce_final={enforce_final}"
                );
                assert!(
                    complete.contains(expected),
                    "complete missing '{expected}' for enforce_final={enforce_final}"
                );
            }
            for forbidden in must_not_contain {
                assert!(
                    !streamed.contains(forbidden),
                    "streaming leaks '{forbidden}' for enforce_final={enforce_final}"
                );
                assert!(
                    !complete.contains(forbidden),
                    "complete leaks '{forbidden}' for enforce_final={enforce_final}"
                );
            }
        }
    }

    #[test]
    fn streaming_and_complete_text_match_byte_by_byte_chunking() {
        // Even with extreme 1-byte chunks, streaming must match the complete
        // path for the fenced-block + final layer.
        let input = "noise\n```json\n{\"tool\":\"a\",\"intent_type\":\"q\",\"payload\":{}}\n```\n<final>answer</final>trail";

        let mut filter = ChatOutputFilter::new(true);
        let mut streamed = String::new();
        for ch in input.chars() {
            let mut buf = [0u8; 4];
            streamed.push_str(&filter.push(ch.encode_utf8(&mut buf)));
        }
        streamed.push_str(&filter.flush());

        let extraction = extract_tool_intent_blocks(input);
        let mut f = FinalTagFilter::new();
        let mut complete = f.push(&extraction.cleaned_text);
        complete.push_str(&f.flush());

        assert_eq!(streamed, complete);
    }
}
