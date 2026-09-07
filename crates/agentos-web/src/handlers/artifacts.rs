//! Agent-authored artifact viewing.
//!
//! `/artifacts/{id}/raw` is the only route in this server allowed to emit
//! agent-authored `text/html`. That is safe *only* because
//! [`crate::router::ARTIFACT_SANDBOX_CSP`] puts the response in a unique opaque
//! origin (`sandbox` **without** `allow-same-origin`) with `default-src 'none'`:
//! no cookie access, no parent DOM, no storage, no network. Read
//! `obsidian-vault/plans/agent-artifacts/Agent Artifacts Security Model.md`
//! before changing anything in this file.
//!
//! Artifacts are ordinary `FileStore` rows written by the `artifact-write` tool
//! and tagged `artifact,kind:<html|markdown|slides>,agent:<id>`. The `artifact`
//! tag is load-bearing: without it a plain uploaded `.html` could be served
//! through the raw route, which is the bypass this whole design exists to close.

use crate::auth::{file_owner_principal, AuthToken};
use crate::file_store::{FileStore, UploadedFile};
use crate::state::AppState;
use axum::extract::{Extension, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

/// Hard read cap, matching the `artifact-write` write cap. A hand-edited registry
/// row must not be able to make the server slurp an arbitrarily large file.
const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024;

/// Slide ceiling for one deck. Guards the input→output amplification in
/// [`build_deck`]; see the comment there.
const MAX_SLIDES: usize = 500;

// ── Page handlers ──────────────────────────────────────────────────────────

/// GET /artifacts — index of agent-generated artifacts.
pub async fn list(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
) -> Response {
    let principal = file_owner_principal(&jar, &headers, &auth);
    let store = Arc::clone(&state.file_store);
    let p = principal.clone();
    let files =
        match tokio::task::spawn_blocking(move || store.list_files(&p, Some("global"))).await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "artifacts::list: db query failed");
                Vec::new()
            }
            Err(e) => {
                tracing::error!(error = %e, "artifacts::list: spawn_blocking panicked");
                Vec::new()
            }
        };

    let mut artifacts = Vec::new();
    for f in files.iter().filter(|f| is_artifact(f)) {
        artifacts.push(context! {
            id          => f.id.clone(),
            name        => f.original_name.clone(),
            kind        => artifact_kind(f).to_string(),
            agent       => agent_label(&state, f).await,
            size_kb     => f.size.saturating_add(1023) / 1024,
            uploaded_at => f.uploaded_at.clone(),
        });
    }

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title  => "Artifacts",
        breadcrumbs => vec![context! { label => "Artifacts" }],
        artifacts,
        csrf_token,
    };
    super::render(&state.templates, "artifacts.html", ctx)
}

/// GET /artifacts/{id} — viewer shell. Served under the **normal app CSP**;
/// `markdown` renders inline here, so it must never contain agent-authored tags.
pub async fn view(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    Path(id): Path<String>,
) -> Response {
    let principal = file_owner_principal(&jar, &headers, &auth);
    // Only `markdown` renders inline here, so only `markdown` needs the bytes —
    // html/slides are fetched by the iframe from /raw.
    let (record, rendered_html) = {
        let (record, _) =
            match load_artifact_row(Arc::clone(&state.file_store), &id, &principal).await {
                Ok(v) => v,
                Err(status) => return status.into_response(),
            };
        match artifact_kind(&record) {
            "markdown" => {
                let (record, content) =
                    match load_artifact(Arc::clone(&state.file_store), &id, &principal).await {
                        Ok(v) => v,
                        Err(status) => return status.into_response(),
                    };
                // Rendered into the app origin — the origin that holds the session
                // cookie and the CSRF meta tag — so raw HTML events are dropped.
                let html = render_markdown(&content, false);
                (record, html)
            }
            "html" | "slides" => (record, String::new()),
            // Unknown kind: no renderer, fail closed rather than guess.
            _ => return StatusCode::NOT_FOUND.into_response(),
        }
    };
    let kind = artifact_kind(&record).to_string();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title  => record.original_name.clone(),
        breadcrumbs => vec![
            context! { label => "Artifacts", href => "/artifacts" },
            context! { label => record.original_name.clone() },
        ],
        artifact    => context! {
            id          => record.id.clone(),
            // Unforgeable half of the provenance banner: the agent NAME is
            // operator-assigned and could read "AgentOS System", the id cannot.
            short_id    => record.id.chars().take(8).collect::<String>(),
            name        => record.original_name.clone(),
            kind        => kind,
            size_kb     => record.size.saturating_add(1023) / 1024,
            uploaded_at => record.uploaded_at.clone(),
        },
        agent       => agent_label(&state, &record).await,
        rendered_html,
        csrf_token,
    };
    super::render(&state.templates, "artifact.html", ctx)
}

/// GET /artifacts/{id}/raw — agent-authored HTML under the sandbox CSP.
///
/// The CSP is applied by `add_security_headers` in the router; do **not** set it
/// here as well or the two sites drift.
pub async fn raw(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    Path(id): Path<String>,
) -> Response {
    let principal = file_owner_principal(&jar, &headers, &auth);
    let (record, content) =
        match load_artifact(Arc::clone(&state.file_store), &id, &principal).await {
            Ok(v) => v,
            Err(status) => return status.into_response(),
        };

    match raw_body(artifact_kind(&record), &record.original_name, &content) {
        Some(body) => html_response(body),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// POST /artifacts/{id}/delete — drop the registry row and the bytes.
pub async fn delete(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    Path(id): Path<String>,
) -> Response {
    let principal = file_owner_principal(&jar, &headers, &auth);
    // Confirm it is an artifact row first, so this route can never delete anything
    // else. The blob is resolved separately and is allowed to be missing — an
    // orphaned row (blob already gone) must still be removable from the index.
    if let Err(status) = load_row_only(Arc::clone(&state.file_store), &id, &principal).await {
        return status.into_response();
    }
    // `None` = no verified path inside uploads_dir, so nothing is unlinked.
    let canonical = load_artifact_row(Arc::clone(&state.file_store), &id, &principal)
        .await
        .ok()
        .map(|(_, path)| path);

    let store = Arc::clone(&state.file_store);
    let fid = id.clone();
    let p = principal.clone();
    match tokio::task::spawn_blocking(move || store.delete_file(&fid, &p)).await {
        Ok(Ok(Some(_))) => match canonical {
            Some(path) => {
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    tracing::warn!(
                        artifact_id = %id, error = %e,
                        "Artifact row deleted but on-disk removal failed — bytes may be orphaned"
                    );
                } else {
                    tracing::info!(artifact_id = %id, "Artifact deleted");
                }
            }
            None => tracing::info!(
                artifact_id = %id,
                "Orphaned artifact row deleted (blob already missing)"
            ),
        },
        Ok(Ok(None)) => {} // already gone
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to delete artifact",
            )
                .into_response()
        }
    }

    Redirect::to("/artifacts").into_response()
}

// ── Loading ────────────────────────────────────────────────────────────────

/// Fetch an artifact-tagged row. No path resolution — callers that need the bytes
/// go through [`load_artifact_row`]; `delete` uses this so a row whose blob has
/// vanished is still removable rather than permanently stuck in the index.
async fn load_row_only(
    store: Arc<FileStore>,
    id: &str,
    owner: &str,
) -> Result<UploadedFile, StatusCode> {
    if Uuid::parse_str(id).is_err() {
        return Err(StatusCode::NOT_FOUND);
    }

    let fid = id.to_string();
    let p = owner.to_string();
    let record = match tokio::task::spawn_blocking(move || store.get_file(&fid, &p)).await {
        Ok(Ok(Some(r))) => r,
        Ok(Ok(None)) => return Err(StatusCode::NOT_FOUND),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "load_artifact: db lookup failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Err(e) => {
            tracing::error!(error = %e, "load_artifact: spawn_blocking panicked");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    // SECURITY: only rows written by `artifact-write` are servable as HTML here.
    // A plain uploaded .html must stay an octet-stream download.
    if !is_artifact(&record) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(record)
}

/// Resolve an artifact row and its verified on-disk path.
///
/// Rejects (as 404, never a panic and never a file read) anything that is not an
/// artifact-tagged row resolving to a path inside `uploads_dir`.
async fn load_artifact_row(
    store: Arc<FileStore>,
    id: &str,
    owner: &str,
) -> Result<(UploadedFile, PathBuf), StatusCode> {
    if Uuid::parse_str(id).is_err() {
        return Err(StatusCode::NOT_FOUND);
    }

    let s = Arc::clone(&store);
    let fid = id.to_string();
    let p = owner.to_string();
    let record = match tokio::task::spawn_blocking(move || s.get_file(&fid, &p)).await {
        Ok(Ok(Some(r))) => r,
        Ok(Ok(None)) => return Err(StatusCode::NOT_FOUND),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "load_artifact: db lookup failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Err(e) => {
            tracing::error!(error = %e, "load_artifact: spawn_blocking panicked");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    // SECURITY: only rows written by `artifact-write` are servable as HTML here.
    // A plain uploaded .html must stay an octet-stream download.
    if !is_artifact(&record) {
        return Err(StatusCode::NOT_FOUND);
    }

    // SECURITY: the stored path must still resolve inside uploads_dir.
    let canonical = PathBuf::from(&record.path)
        .canonicalize()
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let uploads = store
        .uploads_dir
        .canonicalize()
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if !canonical.starts_with(&uploads) {
        tracing::warn!(artifact_id = %record.id, "artifact path escapes uploads_dir");
        return Err(StatusCode::NOT_FOUND);
    }

    Ok((record, canonical))
}

/// Open a path without following a final symlink.
///
/// SECURITY: `load_artifact_row` canonicalizes and prefix-checks the stored path,
/// but canonicalize-then-open is two operations — a link swapped in between them
/// wins the race, and a *hardlink* defeats canonicalization outright (it resolves
/// to itself and passes the prefix check while pointing at another file's inode).
///
/// `O_NOFOLLOW` closes the **final-component** swap race. It does NOT collapse the
/// two calls into one — the canonicalize is still a separate syscall, so a
/// *directory* component swapped for a symlink between them is still followed —
/// and it does not touch the hardlink case, which no path-based check can. That is
/// why `<data_dir>/uploads/` must never be an agent-writable workspace path.
async fn open_nofollow(path: &std::path::Path) -> Result<tokio::fs::File, StatusCode> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        // tokio::fs::OpenOptions exposes custom_flags inherently on unix.
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path).await.map_err(|_| StatusCode::NOT_FOUND)
}

/// [`load_artifact_row`] plus the file contents, capped at [`MAX_ARTIFACT_BYTES`].
async fn load_artifact(
    store: Arc<FileStore>,
    id: &str,
    owner: &str,
) -> Result<(UploadedFile, String), StatusCode> {
    use tokio::io::AsyncReadExt;

    let (record, canonical) = load_artifact_row(store, id, owner).await?;

    let file = open_nofollow(&canonical).await?;
    let mut buf = Vec::new();
    // `take` caps the read itself — a size check on the row or on metadata would
    // be a TOCTOU window.
    file.take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if buf.len() as u64 > MAX_ARTIFACT_BYTES {
        tracing::warn!(artifact_id = %record.id, "artifact exceeds 2 MiB cap");
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let content = String::from_utf8(buf).map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((record, content))
}

// ── Kind dispatch ──────────────────────────────────────────────────────────

/// Body for the raw route, or `None` for kinds that must never be framed.
///
/// `markdown` is deliberately absent: it renders in the viewer under the app CSP,
/// so there is exactly one renderer per kind and no second path to keep in sync.
fn raw_body(kind: &str, title: &str, content: &str) -> Option<String> {
    match kind {
        "html" => Some(content.to_string()),
        "slides" => Some(build_deck(title, content)),
        _ => None,
    }
}

fn html_response(body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        body,
    )
        .into_response()
}

/// An artifact row carries BOTH `artifact` and an `agent:<id>` tag.
///
/// SECURITY: `artifact` alone is not proof of provenance — it is pure
/// alphanumerics, so it survives the upload tag filters in
/// `handlers::files` and `agentos_api::handlers::files`
/// (`retain(|c| c.is_alphanumeric() || " ,-_".contains(c))`) and an operator can
/// type it on an uploaded `.html`. What those filters *do* strip is `:`, so no
/// upload can ever produce `agent:` — requiring it here makes provenance
/// structural instead of resting on one character in a sanitizer two crates away.
fn is_artifact(f: &UploadedFile) -> bool {
    f.tags.iter().any(|t| t == "artifact") && tag_value(f, "agent:").is_some()
}

fn tag_value<'a>(f: &'a UploadedFile, prefix: &str) -> Option<&'a str> {
    f.tags
        .iter()
        .find_map(|t| t.strip_prefix(prefix))
        .filter(|v| !v.is_empty())
}

/// Resolve the `agent:<uuid>` tag to that agent's name for display.
///
/// The provenance banner is anti-phishing chrome — "Generated by agent
/// `3f2b8c1e-…`" tells the operator nothing, so fall back to the raw tag only
/// when the agent is unknown (deleted, or a row written by an older build).
async fn agent_label(state: &AppState, f: &UploadedFile) -> String {
    let Some(tag) = tag_value(f, "agent:") else {
        return "unknown".to_string();
    };
    let Ok(agent_id) = tag.parse::<agentos_types::AgentID>() else {
        return tag.to_string();
    };
    let registry = state.kernel.agent_registry.read().await;
    registry
        .get_by_id(&agent_id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| tag.to_string())
}

fn artifact_kind(f: &UploadedFile) -> &str {
    tag_value(f, "kind:").unwrap_or("")
}

// ── Markdown ───────────────────────────────────────────────────────────────

/// Markdown → HTML. `pulldown-cmark` does not sanitize, so raw HTML in the source
/// passes through — fine for `slides` (opaque-origin iframe, `default-src 'none'`)
/// and **not** fine for `markdown`, which is rendered into the app-origin viewer
/// page next to the session cookie and the CSRF meta tag.
///
/// `allow_raw_html = false` drops every raw-HTML event and neuters non-http(s)
/// link targets, so the only tags in the output are ones pulldown-cmark itself
/// generated.
fn render_markdown(src: &str, allow_raw_html: bool) -> String {
    use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(src, opts);

    let mut out = String::new();
    if allow_raw_html {
        html::push_html(&mut out, parser);
    } else {
        html::push_html(
            &mut out,
            parser.filter_map(|ev| match ev {
                Event::Html(_) | Event::InlineHtml(_) => None,
                // `javascript:` hrefs execute on click: the app CSP carries
                // 'unsafe-inline' on script-src, which permits them. Image
                // destinations get the same treatment for symmetry — inert in
                // current browsers, but this is an allowlist, so it stays closed
                // if that ever changes.
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) if !is_safe_img_src(&dest_url) => Some(Event::Start(Tag::Image {
                    link_type,
                    dest_url: CowStr::Borrowed(""),
                    title,
                    id,
                })),
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) if !is_safe_href(&dest_url) => Some(Event::Start(Tag::Link {
                    link_type,
                    dest_url: CowStr::Borrowed("#"),
                    title,
                    id,
                })),
                other => Some(other),
            }),
        );
    }
    out
}

/// Image sources additionally allow `data:image/…`, which is the documented way to
/// embed pictures in an artifact (the app CSP carries `img-src 'self' data:`, and
/// the sandbox CSP `img-src data: blob:`). `data:` is deliberately NOT allowed for
/// link hrefs, where `data:text/html` is a navigation-to-attacker-document vector.
/// SVG delivered through `<img>` cannot run script, so `data:image/svg+xml` is fine
/// here and would not be through an `<object>`/`<iframe>` — neither of which
/// pulldown-cmark can emit.
fn is_safe_img_src(dest: &str) -> bool {
    let d = dest.trim();
    // Byte comparison, NOT `d[..11]`: `len()` is a byte guard and says nothing
    // about char boundaries, so slicing a `str` there panics on any multibyte
    // destination (`![x](🎉🎉🎉)` puts byte 11 mid-emoji) — reachable from
    // agent-authored markdown, in the synchronous `view` path.
    if d.len() >= 11 && d.as_bytes()[..11].eq_ignore_ascii_case(b"data:image/") {
        return true;
    }
    is_safe_href(d)
}

/// A link target is safe if it carries no URL scheme (relative) or an inert one.
fn is_safe_href(dest: &str) -> bool {
    let d = dest.trim();
    match d.find(':') {
        None => true,
        Some(i) => {
            let scheme = d[..i].to_ascii_lowercase();
            // A ':' after a path separator or query is not a scheme.
            if scheme.contains('/') || scheme.contains('?') || scheme.contains('#') {
                return true;
            }
            matches!(scheme.as_str(), "http" | "https" | "mailto")
        }
    }
}

// ── Slides ─────────────────────────────────────────────────────────────────

/// Split a deck on lines that are exactly `---`, ignoring rules inside fenced
/// code blocks. A naive `split("\n---\n")` shreds any deck containing a code
/// sample with a horizontal rule.
fn split_slides(src: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut pos = 0usize;
    // Open fence as (delimiter char, run length). CommonMark requires the closing
    // run to be at least as long as the opening one, so a ``` inside a ```` block
    // is content, not a close.
    let mut fence: Option<(char, usize)> = None;

    for line in src.split_inclusive('\n') {
        let line_start = pos;
        pos += line.len();
        let trimmed = line.trim();
        // CommonMark allows at most 3 leading columns on a thematic break or a
        // fence; 4+ makes the line an indented code block. Honouring that stops a
        // `---` inside an indented code sample from splitting the deck. A leading
        // TAB expands to column 4, so it must count as code too — counting spaces
        // alone left every tab-indented sample (Makefiles, Go, terminal pastes)
        // splitting the deck.
        let indent: usize = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        let indented_code = indent >= 4;

        let fence_char = if indented_code {
            None
        } else if trimmed.starts_with("```") {
            Some('`')
        } else if trimmed.starts_with("~~~") {
            Some('~')
        } else {
            None
        };
        let run = |c: char| trimmed.chars().take_while(|x| *x == c).count();
        match (fence, fence_char) {
            (None, Some(c)) => fence = Some((c, run(c))),
            // Same delimiter, long enough, and nothing but the delimiter — an
            // info string is only legal on an opening fence.
            (Some((open, n)), Some(c))
                if open == c && run(c) >= n && trimmed.chars().all(|x| x == c) =>
            {
                fence = None
            }
            _ => {}
        }
        if fence.is_none() && fence_char.is_none() && !indented_code && trimmed == "---" {
            out.push(&src[start..line_start]);
            start = pos;
        }
    }
    out.push(&src[start..]);

    out.retain(|s| !s.trim().is_empty());
    if out.is_empty() {
        out.push("");
    }
    out
}

/// Wrap markdown slides in a fully self-contained deck.
///
/// Everything is inline: `default-src 'none'` blocks any CDN, webfont or remote
/// image, so the deck uses the system font stack and images must be `data:` URIs.
/// Inline `<style>`/`<script>` are the only reason this works without a bundler.
fn build_deck(title: &str, markdown_src: &str) -> String {
    let mut slides = split_slides(markdown_src);
    // 2 MiB of `a\n---\n` is ~340k slides, and each emits ~30 bytes of chrome for
    // 6 bytes of input — a ~20 MiB response built in one String. Nobody presents
    // 500 slides; truncate rather than amplify.
    let truncated = slides.len() > MAX_SLIDES;
    if truncated {
        tracing::warn!(
            slides = slides.len(),
            "deck truncated to {MAX_SLIDES} slides"
        );
        slides.truncate(MAX_SLIDES);
    }
    let mut out = String::with_capacity(markdown_src.len() * 2 + DECK_HEAD.len() + DECK_TAIL.len());
    out.push_str(DECK_HEAD);
    out.push_str(&super::files::escape_html_attr(title));
    out.push_str(DECK_STYLE);
    for (i, slide) in slides.iter().enumerate() {
        out.push_str(if i == 0 {
            "<section class=\"slide active\">"
        } else {
            "<section class=\"slide\">"
        });
        out.push_str(&render_markdown(slide, true));
        out.push_str("</section>\n");
    }
    // Tell the reader, not just the server log — a 600-slide deck silently
    // becoming 500 looks like the deck simply ended.
    if truncated {
        out.push_str(&format!(
            "<section class=\"slide\"><h2>Deck truncated</h2><p>Only the first {MAX_SLIDES} \
             slides are shown.</p></section>\n"
        ));
    }
    out.push_str(DECK_TAIL);
    out
}

const DECK_HEAD: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\n\
     <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>";

const DECK_STYLE: &str = r#"</title>
<style>
:root{color-scheme:dark}
*{box-sizing:border-box}
html,body{margin:0;padding:0;height:100%}
body{background:#0d1117;color:#e6edf3;line-height:1.55;
font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,"Noto Sans",sans-serif}
.slide{display:none;min-height:100vh;padding:6vh 8vw 12vh;flex-direction:column;justify-content:center}
.slide.active{display:flex}
.slide>*:first-child{margin-top:0}
.slide h1{font-size:clamp(2rem,6vw,3.5rem);line-height:1.1;margin:0 0 .4em}
.slide h2{font-size:clamp(1.5rem,4vw,2.4rem);margin:0 0 .4em}
.slide h3{font-size:clamp(1.2rem,3vw,1.8rem);margin:0 0 .4em}
.slide p,.slide li{font-size:clamp(1rem,2.2vw,1.5rem)}
.slide ul,.slide ol{padding-left:1.2em}
.slide img{max-width:100%;height:auto}
.slide code{background:rgba(255,255,255,.08);border-radius:4px;padding:.1em .3em;
font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
.slide pre{background:rgba(255,255,255,.06);border-radius:8px;padding:1em;overflow:auto;font-size:.95rem}
.slide pre code{background:none;padding:0}
.slide table{border-collapse:collapse;width:100%}
.slide th,.slide td{border:1px solid rgba(255,255,255,.15);padding:.4em .6em;text-align:left}
.slide blockquote{border-left:4px solid rgba(255,255,255,.25);margin:0;padding-left:1em;opacity:.85}
#nav{position:fixed;left:0;right:0;bottom:0;display:flex;align-items:center;justify-content:center;
gap:1rem;padding:.6rem;background:rgba(13,17,23,.9);font-size:.9rem}
#nav button{background:none;border:1px solid rgba(255,255,255,.25);border-radius:6px;color:inherit;
cursor:pointer;font-size:1.2rem;line-height:1;padding:.1em .7em}
@media print{.slide{display:block!important;page-break-after:always}#nav{display:none}}
</style></head><body>
"#;

const DECK_TAIL: &str = r#"<footer id="nav"><button id="prev" type="button" aria-label="Previous slide">&lsaquo;</button><span id="count"></span><button id="next" type="button" aria-label="Next slide">&rsaquo;</button></footer>
<script>
(function(){
var slides=document.querySelectorAll('.slide');
var count=document.getElementById('count');
var i=0;
function show(n){
if(!slides.length)return;
if(n<0)n=0;
if(n>slides.length-1)n=slides.length-1;
i=n;
for(var k=0;k<slides.length;k++){slides[k].className=(k===i)?'slide active':'slide';}
count.textContent=(i+1)+' of '+slides.length;
}
document.addEventListener('keydown',function(e){
if(e.key==='ArrowRight'||e.key==='PageDown'||e.key===' '){e.preventDefault();show(i+1);}
else if(e.key==='ArrowLeft'||e.key==='PageUp'){e.preventDefault();show(i-1);}
else if(e.key==='Home'){show(0);}
else if(e.key==='End'){show(slides.length-1);}
});
document.getElementById('prev').addEventListener('click',function(){show(i-1);});
document.getElementById('next').addEventListener('click',function(){show(i+1);});
show(0);
})();
</script>
</body></html>"#;

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Phase 2: loader + response shape ───────────────────────────────────

    fn seed(store: &FileStore, id: &str, tags: &str, body: &str, path: Option<PathBuf>) {
        let path = path.unwrap_or_else(|| {
            let dir = store.uploads_dir.join("artifacts");
            std::fs::create_dir_all(&dir).expect("artifacts dir");
            dir.join(format!("{id}.html"))
        });
        std::fs::write(&path, body).expect("write artifact");
        let path_str = path.to_string_lossy().to_string();
        store
            .register_file(
                id,
                "report.html",
                "text/html",
                body.len() as u64,
                &path_str,
                tags,
                "",
                "global",
            )
            .expect("register");
    }

    #[tokio::test]
    async fn load_artifact_reads_tagged_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileStore::open(dir.path()).expect("store"));
        let id = "aaaaaaaa-0000-0000-0000-00000000a001";
        seed(
            &store,
            id,
            "artifact,kind:html,agent:writer",
            "<h1>hi</h1>",
            None,
        );

        let (record, content) = load_artifact(Arc::clone(&store), id, "")
            .await
            .expect("artifact loads");
        assert_eq!(content, "<h1>hi</h1>");
        assert_eq!(artifact_kind(&record), "html");
        assert_eq!(tag_value(&record, "agent:"), Some("writer"));
    }

    /// A plain uploaded `.html` must never be servable through the raw route —
    /// that is the whole XSS bypass this design closes.
    #[tokio::test]
    async fn raw_rejects_non_artifact_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileStore::open(dir.path()).expect("store"));
        let id = "aaaaaaaa-0000-0000-0000-00000000a002";
        seed(&store, id, "uploaded,report", "<script>x()</script>", None);

        let err = load_artifact(Arc::clone(&store), id, "")
            .await
            .expect_err("plain upload must not load as an artifact");
        assert_eq!(err, StatusCode::NOT_FOUND);
    }

    /// `artifact` is plain alphanumerics, so it survives the upload tag filters
    /// and an operator can type it on an uploaded `.html`. Provenance must come
    /// from `agent:`, which those filters CANNOT produce (they strip `:`).
    #[tokio::test]
    async fn raw_rejects_upload_that_merely_claims_the_artifact_tag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileStore::open(dir.path()).expect("store"));
        let id = "aaaaaaaa-0000-0000-0000-00000000a005";
        seed(
            &store,
            id,
            "artifact,kindhtml",
            "<script>x()</script>",
            None,
        );

        let err = load_artifact(Arc::clone(&store), id, "")
            .await
            .expect_err("self-declared artifact tag must not confer provenance");
        assert_eq!(err, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn path_escape_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let store = Arc::new(FileStore::open(dir.path()).expect("store"));
        let id = "aaaaaaaa-0000-0000-0000-00000000a003";
        // Hand-written row pointing outside uploads_dir.
        let escaped = outside.path().join("secret.html");
        seed(
            &store,
            id,
            "artifact,kind:html,agent:writer",
            "TOPSECRET",
            Some(escaped),
        );

        let err = load_artifact(Arc::clone(&store), id, "")
            .await
            .expect_err("path outside uploads_dir must 404");
        assert_eq!(err, StatusCode::NOT_FOUND);
    }

    #[test]
    fn raw_content_type_is_html() {
        let resp = html_response("<h1>x</h1>".to_string());
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("private, no-store")
        );
    }

    /// No integration harness exists in this crate (`AppState` needs a booted
    /// kernel), so this guards the invariant at its source: the auth-bypass list
    /// in `require_auth` must never name `/artifacts`.
    #[test]
    fn raw_requires_auth() {
        let src = include_str!("../auth.rs");
        assert!(
            !src.contains("artifacts"),
            "/artifacts must not appear in the auth bypass allowlist"
        );
    }

    /// Regression guard against "unifying" the download policy with the artifact
    /// policy: `/files/{id}/download` must keep forcing octet-stream for HTML.
    /// Asserted over the source because `safe_download_mime` is private to
    /// `files.rs` and that file is intentionally not modified.
    #[test]
    fn download_mime_unchanged() {
        let src = include_str!("files.rs");
        let start = src
            .find("fn safe_download_mime")
            .expect("safe_download_mime must exist");
        let body = &src[start..];
        let end = body.find("\n}").expect("function must terminate");
        let body = &body[..end];
        assert!(
            !body.contains("html"),
            "safe_download_mime must never allowlist an HTML mime"
        );
        assert!(body.contains("application/octet-stream"));
    }

    // ── Phase 3: markdown ──────────────────────────────────────────────────

    #[test]
    fn markdown_strips_raw_html() {
        // Block form: the whole tag becomes one Html event and disappears.
        let out = render_markdown("Hello\n\n<script>alert(1)</script>\n", false);
        assert!(!out.contains("<script"), "script tag survived: {out}");
        assert!(!out.contains("alert"), "script body survived: {out}");

        // Inline form: the tags are dropped; the leftover literal text is inert
        // (it is text content, not markup).
        let out = render_markdown("Hello <script>alert(1)</script>", false);
        assert!(!out.contains("<script"), "inline script survived: {out}");
        assert!(!out.contains("<img"), "no raw tags may survive: {out}");

        // The same source under allow_raw_html keeps the tag — that path is only
        // reachable inside the opaque-origin iframe.
        let out = render_markdown("Hello\n\n<script>alert(1)</script>\n", true);
        assert!(out.contains("<script"));
    }

    #[test]
    fn markdown_neuters_javascript_links() {
        let out = render_markdown("[click](javascript:alert(1))", false);
        assert!(!out.contains("javascript:"), "js href survived: {out}");
        assert!(out.contains("href=\"#\""), "link not neutered: {out}");

        let out = render_markdown("[ok](https://example.com/x)", false);
        assert!(out.contains("https://example.com/x"));
    }

    #[test]
    fn markdown_image_sources_are_scheme_checked() {
        // data:image is the documented way to embed a picture and must survive.
        let out = render_markdown("![x](data:image/png;base64,iVBORw0KGgo=)", false);
        assert!(out.contains("data:image/png"), "data image blanked: {out}");

        // Anything else inert-but-unexpected is blanked. `is_safe_img_src` is an
        // allowlist, so obfuscation fails closed rather than open.
        for src in [
            "![x](javascript:alert(1))",
            "![x](JaVaScRiPt:alert(1))",
            "![x](data:text/html,<script>alert(1)</script>)",
        ] {
            let out = render_markdown(src, false);
            assert!(!out.contains("javascript"), "js img src survived: {out}");
            assert!(!out.contains("text/html"), "html data uri survived: {out}");
        }
    }

    #[test]
    fn multibyte_destinations_do_not_panic() {
        // Byte 11 lands mid-emoji: a `str` slice here panics, and this input is
        // agent-authored and reaches the synchronous `view` path.
        for src in ["![x](🎉🎉🎉)", "[l](🎉🎉🎉)", "![x](aaaaaaaaaaé)"] {
            let _ = render_markdown(src, false);
            let _ = render_markdown(src, true);
        }
    }

    #[test]
    fn deck_slide_count_is_capped() {
        // Input→output amplification guard: 6 bytes in, ~30 bytes of chrome out.
        let src = "a\n---\n".repeat(MAX_SLIDES + 250);
        let deck = build_deck("t", &src);
        // MAX_SLIDES real slides + the truncation notice.
        assert_eq!(
            deck.matches("<section class=\"slide").count(),
            MAX_SLIDES + 1,
            "slide cap not enforced"
        );
        assert!(deck.contains("Deck truncated"), "truncation not disclosed");

        // An exactly-at-cap deck is not truncated and gets no notice.
        let exact = build_deck("t", &"a\n---\n".repeat(MAX_SLIDES));
        assert!(!exact.contains("Deck truncated"), "false truncation notice");
    }

    #[test]
    fn markdown_renders_tables() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let out = render_markdown(src, false);
        assert!(out.contains("<table>"), "no table in output: {out}");
        assert!(out.contains("<td>1</td>"), "no cells in output: {out}");
    }

    // ── Phase 3: slides ────────────────────────────────────────────────────

    #[test]
    fn split_slides_basic() {
        let slides = split_slides("a\n---\nb\n---\nc");
        assert_eq!(slides.len(), 3, "got {slides:?}");
        assert_eq!(slides[0].trim(), "a");
        assert_eq!(slides[1].trim(), "b");
        assert_eq!(slides[2].trim(), "c");
    }

    #[test]
    fn split_slides_ignores_fenced_rule() {
        let src = "intro\n\n```\nfoo\n---\nbar\n```\n\noutro\n";
        let slides = split_slides(src);
        assert_eq!(slides.len(), 1, "fenced rule split the deck: {slides:?}");

        // Tilde fences too, and a real break outside the fence still counts.
        let src = "one\n\n~~~\n---\n~~~\n\n---\n\ntwo\n";
        let slides = split_slides(src);
        assert_eq!(slides.len(), 2, "got {slides:?}");
        assert!(slides[0].contains("~~~"));
        assert!(slides[1].contains("two"));
    }

    #[test]
    fn split_slides_ignores_indented_code_rule() {
        // 4+ spaces = indented code block, so this `---` is sample content, not a
        // slide break. 3 spaces is still a thematic break per CommonMark.
        let slides = split_slides("intro\n\n    foo\n    ---\n    bar\n\noutro\n");
        assert_eq!(slides.len(), 1, "indented rule split the deck: {slides:?}");

        let slides = split_slides("a\n\n   ---\n\nb\n");
        assert_eq!(slides.len(), 2, "3-space rule must still split: {slides:?}");

        // A leading TAB expands to column 4 — same rule. Counting spaces only
        // left every tab-indented sample splitting the deck.
        let slides = split_slides("intro\n\n\tfoo\n\t---\n\tbar\n\noutro\n");
        assert_eq!(
            slides.len(),
            1,
            "tab-indented rule split the deck: {slides:?}"
        );
    }

    #[test]
    fn split_slides_requires_matching_fence_length() {
        // ``` cannot close a ```` block (CommonMark: closing run >= opening run).
        // Treating it as a close reopens a fence that then swallows the real
        // break, silently collapsing the deck to one slide.
        let slides = split_slides("````\n---\n```\n````\n\n---\n\ntwo\n");
        assert_eq!(slides.len(), 2, "short fence closed a long one: {slides:?}");
        assert!(slides[1].contains("two"));

        // An info string is legal only on an opening fence, so ```rust does not
        // close an open ``` block.
        let slides = split_slides("```\n---\n```rust\n---\n```\n\n---\n\ntwo\n");
        assert_eq!(
            slides.len(),
            2,
            "info-string line closed a fence: {slides:?}"
        );
    }

    #[test]
    fn deck_is_self_contained() {
        let deck = build_deck("Deck", "# One\n\n---\n\n# Two\n");
        assert!(!deck.contains("http:"), "external scheme in deck");
        assert!(!deck.contains("https:"), "external scheme in deck");
        // Also catches scheme-relative `//host` — the generated chrome contains
        // no `//` at all, so any occurrence means a URL crept in.
        assert!(!deck.contains("//"), "scheme-relative URL in deck");
        assert!(deck.contains("<section class=\"slide active\">"));
        assert_eq!(deck.matches("<section class=").count(), 2);
    }

    #[test]
    fn deck_escapes_title() {
        let deck = build_deck("</title><script>alert(1)</script>", "hi");
        assert!(
            deck.contains("&lt;/title&gt;&lt;script&gt;"),
            "title not escaped: {}",
            &deck[..200.min(deck.len())]
        );
        assert!(!deck.contains("</title><script>"));
    }

    #[test]
    fn slides_raw_serves_html_and_markdown_is_404() {
        let body = raw_body("slides", "Deck", "# One\n---\n# Two\n").expect("slides render");
        assert!(body.starts_with("<!doctype html>"));
        assert!(body.contains("<h1>One</h1>"));

        assert_eq!(
            raw_body("html", "Page", "<b>x</b>").as_deref(),
            Some("<b>x</b>")
        );
        // markdown renders in the viewer under the app CSP, never in the frame.
        assert!(raw_body("markdown", "Doc", "# hi").is_none());
        assert!(raw_body("", "Doc", "# hi").is_none());
        assert!(raw_body("exe", "Doc", "# hi").is_none());
    }
}
