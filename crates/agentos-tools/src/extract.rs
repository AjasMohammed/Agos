//! Best-effort conversion of arbitrary files into agent-readable UTF-8 text.
//!
//! Every path that hands a file to an agent — chat attachments, `@filename`
//! mentions, `user-file-reader`, `file-reader` — routes through
//! [`read_as_text`]. Without it, anything that is not a declared text MIME
//! reaches the model as base64, which it cannot read (observed in production:
//! an agent burned five iterations and ~18k tokens shelling out to a missing
//! `PyPDF2` to decode a PDF).
//!
//! Conversion uses external binaries that are standard on a Linux desktop
//! (`pdftotext`/`pdftoppm` from poppler, `soffice` from LibreOffice) rather
//! than per-format Rust crates: two binaries cover ~40 formats. A missing
//! binary is never an error — extraction just declines and the caller keeps
//! its existing binary handling.

use crate::sandbox_fs;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Maximum extracted text handed back to a caller. Longer output is truncated
/// with an explicit marker so the agent knows it is seeing a prefix.
pub const MAX_TEXT_BYTES: usize = 5 * 1024 * 1024;

/// Wall-clock cap for any single external converter invocation. LibreOffice
/// cold start is 3–8s; anything past this is a malformed document, and an
/// agent must never hang on one.
pub const CONVERT_TIMEOUT: Duration = Duration::from_secs(20);

/// Bytes inspected when guessing whether an undeclared file is really text.
const SNIFF_BYTES: usize = 64 * 1024;

/// Files larger than this are never handed to an external converter. Bounds
/// how long one document can occupy a converter process (uploads are allowed up
/// to 100 MiB, which LibreOffice would chew on for the whole timeout).
const MAX_CONVERT_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Concurrent LibreOffice conversions across the process. Each `soffice` is
/// ~250 MB resident, and one chat turn can attach 20 documents, so an unbounded
/// fan-out is a memory incident.
const MAX_CONCURRENT_OFFICE: usize = 2;

/// Concurrent `html2text` parses. The DOM build runs on an uncancellable
/// blocking thread, and that pool is shared with every `tokio::fs` and SQLite
/// call in the workspace — saturating it stops the process serving.
const MAX_CONCURRENT_HTML: usize = 4;

/// Markup handed to `html2text`. Well past any real page, and small enough that
/// a full pool of parses cannot hold gigabytes resident.
const MAX_HTML_INPUT_BYTES: usize = 2 * 1024 * 1024;

/// Rendered PDF pages are capped on the long edge instead of trusting the DPI:
/// a poster-sized `MediaBox` at 150 dpi is a multi-gigabyte PNG.
const MAX_PAGE_PIXELS: &str = "2000";

/// How long to wait for a failed converter's stderr to reach EOF before giving
/// up and killing its process group. Short: this runs after the child is
/// already reaped, so anything still holding the pipe is a survivor.
const STDERR_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Size of the sandbox `/tmp`. Bounded because converters spool there and a
/// bare tmpfs defaults to half of host RAM.
const TMPFS_BYTES: &str = "268435456"; // 256 MiB

/// Fraction of control characters above which a sniffed file is treated as
/// binary rather than text.
const MAX_CONTROL_RATIO: f32 = 0.05;

/// External converter selected for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Converter {
    /// Extract the text layer with `pdftotext`.
    PdfText,
    /// Convert with headless LibreOffice. Spreadsheets export to CSV so cell
    /// structure survives; everything else exports to plain text.
    Office { spreadsheet: bool },
    /// Strip markup in-process with `html2text`.
    Html,
}

/// True for MIME types whose bytes are already UTF-8 text.
///
/// Structured suffixes are matched with `+xml`/`+json` rather than a bare
/// substring: `application/vnd.openxmlformats-officedocument.…` (a `.docx`)
/// contains "xml" but is a ZIP archive, and inlining it as text puts binary
/// garbage in the model's context.
pub fn is_text_mime(mime: &str) -> bool {
    let m = mime.to_ascii_lowercase();
    m.starts_with("text/")
        || m == "application/json"
        || m == "application/xml"
        || m.ends_with("+json")
        || m.ends_with("+xml")
        || m.contains("javascript")
        || m.contains("yaml")
        || m.contains("toml")
        || m.contains("markdown")
}

/// Pick a converter for a MIME type / file extension pair, without touching
/// the file. Either signal alone is unreliable: browsers send `.docx` as
/// `application/octet-stream`, and channel uploads often carry no extension.
pub fn converter_for(mime: &str, ext: &str) -> Option<Converter> {
    let m = mime.to_ascii_lowercase();
    let e = ext.to_ascii_lowercase();

    const SHEET_EXT: &[&str] = &["xls", "xlsx", "xlsm", "ods", "fods"];
    // No `pages`/`numbers`/`key`: LibreOffice has no iWork import filter, and
    // `.key` in the wild is overwhelmingly a PEM secret, not a Keynote deck.
    // No `wpd`/`abw` either: libwpd and libabw are LibreOffice's least
    // maintained import filters and the converter is not sandboxed (see the
    // module docs), so the long tail is not worth the parser surface.
    const DOC_EXT: &[&str] = &[
        "doc", "docx", "docm", "odt", "fodt", "rtf", "ppt", "pptx", "odp", "fodp",
    ];
    // Windows registers `.csv` as `application/vnd.ms-excel`. Round-tripping a
    // CSV through LibreOffice mangles it (leading zeros dropped, dates
    // relocalized) — inline these verbatim instead. Checked first because it is
    // the one case where the extension is the more trustworthy signal.
    const PLAIN_TABULAR_EXT: &[&str] = &["csv", "tsv", "txt"];

    if PLAIN_TABULAR_EXT.contains(&e.as_str()) {
        return None;
    }

    let sheet_mime = m.contains("spreadsheet")
        || m.contains("ms-excel")
        || m.contains("excel")
        || m.contains("opendocument.spreadsheet");
    // No `epub`: LibreOffice exports EPUB but cannot import it, so routing one
    // here buys a 20s timeout instead of a fast decline.
    let doc_mime = m.contains("wordprocessing")
        || m.contains("msword")
        || m.contains("ms-word")
        || m.contains("opendocument.text")
        || m.contains("opendocument.presentation")
        || m.contains("presentation")
        || m.contains("ms-powerpoint")
        || m.contains("powerpoint")
        || m.contains("rtf");

    // MIME outranks extension for every format: `report.htm` served as
    // `application/pdf` is a PDF, and letting the extension win would hand it
    // to html2text, which declines — losing a file `pdftotext` reads fine.
    if m.contains("html") {
        return Some(Converter::Html);
    }
    if m.contains("pdf") {
        return Some(Converter::PdfText);
    }
    if sheet_mime {
        return Some(Converter::Office { spreadsheet: true });
    }
    if doc_mime {
        return Some(Converter::Office { spreadsheet: false });
    }

    if matches!(e.as_str(), "html" | "htm" | "xhtml") {
        return Some(Converter::Html);
    }
    if e == "pdf" {
        return Some(Converter::PdfText);
    }
    if SHEET_EXT.contains(&e.as_str()) {
        return Some(Converter::Office { spreadsheet: true });
    }
    if DOC_EXT.contains(&e.as_str()) {
        return Some(Converter::Office { spreadsheet: false });
    }
    None
}

/// Text produced from a file, and how it was produced.
#[derive(Debug, Clone)]
pub struct Extracted {
    /// The agent-readable text.
    pub text: String,
    /// True when a converter rewrote the bytes (PDF text layer, LibreOffice
    /// export, markup stripped). False means `text` is the file verbatim.
    ///
    /// An agent told its content is verbatim when it is not will write patches
    /// that never apply; told it is converted when it is not, it will refuse to
    /// edit a file it could edit. Both are worth one bool.
    pub converted: bool,
}

/// Convert `path` to agent-readable text, reporting whether a converter ran.
///
/// Returns `None` when the file belongs on another path (images go to the
/// vision pipeline) or cannot be converted — callers keep their existing
/// binary/base64 handling in that case. Never returns an error: a failed
/// conversion is not a tool failure.
pub async fn extract_text(path: &Path, mime: &str) -> Option<Extracted> {
    // Converters take the path as an argv element. Every caller in this
    // workspace canonicalizes first, and an absolute path can never be read as
    // an option by a converter that does not honour `--`.
    if !path.is_absolute() {
        tracing::warn!(path = %path.display(), "extract: refusing relative path");
        return None;
    }

    let mime_lc = mime.to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Raster/audio/video belong to the vision path. Checked before the
    // converters so a `photo.htm` cannot be dragged onto the HTML branch by its
    // extension. `image/svg+xml` is exempt: it is markup, and inlining it beats
    // declining it.
    let is_media = mime_lc.starts_with("image/")
        || mime_lc.starts_with("audio/")
        || mime_lc.starts_with("video/");
    if is_media && !is_text_mime(&mime_lc) {
        return None;
    }

    // Converters run before the text check: `text/html` is technically text but
    // reads better stripped, and a `.docx` MIME carries "xml" while the bytes
    // are a ZIP archive.
    let converter = converter_for(&mime_lc, &ext);

    // The size gate is keyed on "a converter would run", not on the MIME
    // family: `image/svg+xml` is both media and text, so gating on `!is_media`
    // let a 100 MiB upload declared `image/svg+xml` and named `.docx` walk
    // straight into LibreOffice.
    if converter.is_some() && oversized_for_conversion(path).await {
        // Too big to convert. Declared text is still read directly — the sniff
        // would reject a huge latin-1 log outright — but only after a NUL
        // check, since the declaration is the attacker's to choose.
        if is_text_mime(&mime_lc) {
            return read_declared_text(path).await.map(verbatim);
        }
        return sniff_text(path).await.map(verbatim);
    }

    match converter {
        Some(Converter::Html) => {
            if let Some(text) = html_to_text(path).await {
                return Some(Extracted {
                    text,
                    converted: true,
                });
            }
            // Markup that renders to nothing (an SPA shell, say) is still worth
            // showing verbatim — fall through to the text branch.
        }
        Some(Converter::PdfText) => {
            if let Some(text) = pdf_to_text(path).await {
                return Some(Extracted {
                    text,
                    converted: true,
                });
            }
            // Empty text layer = scanned PDF. Caller may rasterize it for the
            // vision path (see `rasterize_pdf`).
            return None;
        }
        Some(Converter::Office { spreadsheet }) => {
            if let Some(text) = office_to_text(path, spreadsheet).await {
                return Some(Extracted {
                    text,
                    converted: true,
                });
            }
            // Fall through: some `.doc` files are really plain text with a
            // misleading name, so the text check and sniff still get a turn.
        }
        None => {}
    }

    // `read_declared_text`, not `read_capped_text`: the MIME is chosen by
    // whoever uploaded the file, so a 1 KiB "text/plain" of NUL bytes must not
    // reach the model just because it was too small to hit the oversize branch
    // that does check.
    if is_text_mime(&mime_lc) {
        return read_declared_text(path).await.map(verbatim);
    }

    sniff_text(path).await.map(verbatim)
}

/// [`extract_text`] for callers that only need the text.
pub async fn read_as_text(path: &Path, mime: &str) -> Option<String> {
    extract_text(path, mime).await.map(|e| e.text)
}

fn verbatim(text: String) -> Extracted {
    Extracted {
        text,
        converted: false,
    }
}

/// True when the file is too large to be worth handing to an external
/// converter. A converter's own output is capped, but LibreOffice parsing a
/// 100 MiB document ties up a process for the full timeout.
async fn oversized_for_conversion(path: &Path) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.len() > MAX_CONVERT_INPUT_BYTES => {
            tracing::warn!(
                path = %path.display(),
                bytes = m.len(),
                "extract: file too large to convert, falling back to text sniff"
            );
            true
        }
        _ => false,
    }
}

/// Strip markup with `html2text`.
///
/// The DOM build runs on a blocking thread that cannot be cancelled, so both
/// bounds available are applied up front: how much it is given, and how many
/// may run at once. Without the second, twenty pathological uploads pin twenty
/// blocking threads — a pool the workspace's SQLite calls also share — with no
/// way to reclaim them.
async fn html_to_text(path: &Path) -> Option<String> {
    static HTML_SLOTS: OnceLock<Semaphore> = OnceLock::new();
    let _permit = HTML_SLOTS
        .get_or_init(|| Semaphore::new(MAX_CONCURRENT_HTML))
        .acquire()
        .await
        .ok()?;

    let bytes = read_head(path, MAX_HTML_INPUT_BYTES + 1).await?;
    if !looks_texty(&bytes) {
        return None;
    }
    let input_capped = bytes.len() > MAX_HTML_INPUT_BYTES;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let mut text = tokio::task::spawn_blocking(move || html2text::from_read(raw.as_bytes(), 100))
        .await
        .ok()?;
    if input_capped {
        // The output is smaller than the input, so the generic cap never fires
        // here — mark it explicitly or the model is handed a prefix of a page
        // and told it is the page.
        text.push_str(&truncation_marker(
            MAX_HTML_INPUT_BYTES,
            file_len(path).await,
        ));
    }
    non_empty(truncate_text(text, None))
}

/// Extract a PDF's text layer. `None` when poppler is absent or the PDF has no
/// text (scanned scans/photos).
async fn pdf_to_text(path: &Path) -> Option<String> {
    let (out, hit_cap) = run_capture(
        "pdftotext",
        &[
            OsString::from("-layout"),
            OsString::from("-q"),
            OsString::from("-enc"),
            OsString::from("UTF-8"),
            OsString::from("--"),
            path.as_os_str().to_os_string(),
            OsString::from("-"),
        ],
        &Sandbox {
            input: path,
            work: None,
        },
    )
    .await?;
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if hit_cap {
        // Length of the source PDF is not the length of its text layer, so
        // there is no honest total to report — say only that it is a prefix.
        text = truncate_text(text, None);
    }
    non_empty(text)
}

/// Convert an office document via headless LibreOffice.
async fn office_to_text(path: &Path, spreadsheet: bool) -> Option<String> {
    // ponytail: LibreOffice's CSV export writes only the first sheet of a
    // workbook. Upgrade path if multi-sheet matters: loop the sheet index with
    // a per-sheet `--convert-to csv` and concatenate with a sheet header.
    let filter = if spreadsheet {
        // field separator 44 (`,`), text delimiter 34 (`"`), charset 76 (UTF-8).
        "csv:Text - txt - csv (StarCalc):44,34,76"
    } else {
        "txt:Text (encoded):UTF8"
    };

    // Bound how many LibreOffice processes exist at once; one chat turn can
    // attach 20 documents. The wait itself is bounded too: an unbounded
    // `acquire()` behind two slots lets a handful of slow documents queue every
    // later request forever.
    static OFFICE_SLOTS: OnceLock<Semaphore> = OnceLock::new();
    let _permit = tokio::time::timeout(
        CONVERT_TIMEOUT,
        OFFICE_SLOTS
            .get_or_init(|| Semaphore::new(MAX_CONCURRENT_OFFICE))
            .acquire(),
    )
    .await
    .ok()?
    .ok()?;

    // Each invocation gets a private profile: the default LibreOffice profile
    // is single-instance-locked, so concurrent agent reads would serialize or
    // fail against a shared one.
    let work = private_temp_dir("agentos-soffice").await?;
    let profile = work.path().join("profile");
    let outdir = work.path().join("out");
    // `-env:UserInstallation=` takes a URL, so a temp root containing `%`, `#`
    // or a space resolves to a *different* directory than the one seeded below.
    // LibreOffice then bootstraps a fresh, unhardened profile there and the
    // conversion still succeeds — link fetching back on, with nothing to show
    // for it. Refuse instead of silently losing the hardening.
    let Some(profile_url) = file_url(&profile) else {
        tracing::warn!(
            profile = %profile.display(),
            "extract: temp dir is not safe in a file:// URL; skipping office conversion"
        );
        return None;
    };
    seed_office_profile(&profile).await?;

    let result = run_capture(
        "soffice",
        &[
            OsString::from("--headless"),
            OsString::from("--norestore"),
            OsString::from("--invisible"),
            OsString::from(format!("-env:UserInstallation={profile_url}")),
            OsString::from("--convert-to"),
            OsString::from(filter),
            OsString::from("--outdir"),
            outdir.as_os_str().to_os_string(),
            path.as_os_str().to_os_string(),
        ],
        &Sandbox {
            input: path,
            work: Some(work.path()),
        },
    )
    .await;

    let text = if result.is_some() {
        read_first_file(&outdir).await
    } else {
        None
    };

    // `work` removes itself on drop, including when this future is cancelled
    // mid-conversion. No truncation here: `read_first_file` went through
    // `read_capped_text`, which already capped and marked with the real byte
    // total — re-truncating only replaces that honest total with a vaguer note.
    text.and_then(non_empty)
}

/// Pre-seed the throwaway LibreOffice profile so the import runs with remote
/// fetching and macros off.
///
/// There is no CLI flag for either — the settings live in the user profile, and
/// a fresh profile means the build's defaults apply, which is link-fetching
/// **on**. Without this, a `.ods` containing `=WEBSERVICE("http://169.254.169.254/…")`
/// or an ODT with a remote `xlink:href` turns any upload into an SSRF from the
/// kernel's network position. The manifests' `network = false` does not help:
/// core-tier tools run in-process, so no seccomp filter is ever built.
async fn seed_office_profile(profile: &Path) -> Option<()> {
    const REGISTRY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
 <item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item>
 <item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>
 <item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="LinkUpdateMode" oor:op="fuse"><value>0</value></prop></item>
 <item oor:path="/org.openoffice.Office.Calc/Content/Update"><prop oor:name="Link" oor:op="fuse"><value>2</value></prop></item>
</oor:items>
"#;
    let user = profile.join("user");
    if let Err(e) = tokio::fs::create_dir_all(&user).await {
        tracing::warn!(error = %e, "extract: cannot create soffice profile dir");
        return None;
    }
    if let Err(e) = tokio::fs::write(user.join("registrymodifications.xcu"), REGISTRY).await {
        tracing::warn!(error = %e, "extract: cannot seed soffice profile");
        return None;
    }
    Some(())
}

/// Read the single file LibreOffice wrote into `dir`.
async fn read_first_file(dir: &Path) -> Option<String> {
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        // Skip unreadable entries rather than abandoning the scan.
        if entry
            .file_type()
            .await
            .map(|t| t.is_file())
            .unwrap_or(false)
        {
            return read_capped_text(&entry.path()).await;
        }
    }
    None
}

/// A converter's working directory, removed when it goes out of scope.
///
/// Removal must be a `Drop`, not a trailing `remove_dir_all().await`: these
/// futures run inline in an axum handler, so a client disconnect drops them at
/// an await point and any cleanup written after that point never runs. What
/// leaks is the source document's plaintext, forever, in `/tmp`.
struct WorkDir(PathBuf);

impl WorkDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Create a private, owner-only working directory under the system temp dir.
/// Converters write plaintext of the source document into it, so `0700` matters
/// on a shared host.
async fn private_temp_dir(prefix: &str) -> Option<WorkDir> {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    let d = dir.clone();
    let created = tokio::task::spawn_blocking(move || {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&d)
    })
    .await
    .ok()?;
    created.ok()?;
    Some(WorkDir(dir))
}

/// Page count of a PDF, so callers can tell an agent how much of a document it
/// is actually looking at. `None` when `pdfinfo` is unavailable.
pub async fn pdf_page_count(path: &Path) -> Option<usize> {
    if !path.is_absolute() {
        return None;
    }
    let (out, _) = run_capture(
        "pdfinfo",
        &[OsString::from("--"), path.as_os_str().to_os_string()],
        &Sandbox {
            input: path,
            work: None,
        },
    )
    .await?;
    // Last match, not first: `pdfinfo` prints `Title:` above `Pages:`, and a
    // PDF whose title contains a newline followed by `Pages: 9999` would
    // otherwise dictate the page count reported to the model.
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.strip_prefix("Pages:"))
        .filter_map(|v| v.trim().parse().ok())
        .next_back()
}

/// Render up to `max_pages` pages of a PDF to PNGs, paired with their real page
/// numbers.
///
/// Used for PDFs with no text layer: the model already accepts images, so a
/// scanned document becomes a vision problem instead of a dead end. Returns an
/// empty vec when `pdftoppm` is missing or nothing rendered.
///
/// The page number is returned rather than left to the caller's loop index: a
/// page that renders too large is skipped, so position in the result is not
/// position in the document, and a model that cites page 1 for page 2 is worse
/// than one that cites nothing.
pub async fn rasterize_pdf(
    path: &Path,
    max_pages: usize,
    max_page_bytes: usize,
) -> Vec<(usize, Vec<u8>)> {
    if max_pages == 0 || !path.is_absolute() {
        return Vec::new();
    }
    let Some(work) = private_temp_dir("agentos-pdfpage").await else {
        return Vec::new();
    };
    let stem = work.path().join("page");

    let ran = run_capture(
        "pdftoppm",
        &[
            OsString::from("-png"),
            // `-scale-to` instead of `-r`: DPI on an oversized MediaBox renders
            // a page no adapter would accept and no host should allocate.
            OsString::from("-scale-to"),
            OsString::from(MAX_PAGE_PIXELS),
            OsString::from("-f"),
            OsString::from("1"),
            OsString::from("-l"),
            OsString::from(max_pages.to_string()),
            OsString::from("--"),
            path.as_os_str().to_os_string(),
            stem.as_os_str().to_os_string(),
        ],
        &Sandbox {
            input: path,
            work: Some(work.path()),
        },
    )
    .await;

    if ran.is_none() {
        tracing::debug!("extract: pdftoppm failed or timed out; using whatever it rendered");
    }

    let mut pages: Vec<(usize, Vec<u8>)> = Vec::new();
    // Scanned whatever the exit code was: `pdftoppm` writes pages incrementally
    // and exits non-zero if a *later* page fails, so gating on success threw
    // away every page of a document that rendered all but its last one. The
    // per-file checks below already reject anything unusable.
    {
        if let Ok(mut entries) = tokio::fs::read_dir(work.path()).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let p = entry.path();
                // Regular files only, and checked with the non-following
                // `file_type` before anything opens the path. The work dir is
                // bound writable into the converter's sandbox, so a compromised
                // `pdftoppm` can drop `page-01.png` as a symlink to any host
                // file; `metadata()` would size the link itself while the read
                // below follows it — outside the namespace — and the bytes end
                // up stored as an image and sent to the model.
                if !entry
                    .file_type()
                    .await
                    .map(|t| t.is_file())
                    .unwrap_or(false)
                {
                    continue;
                }
                if p.extension().and_then(|e| e.to_str()) != Some("png") {
                    continue;
                }
                // `pdftoppm` names output `page-01.png`, zero-padded to the
                // width of the last page it was asked for.
                let Some(page_no) = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.rsplit('-').next())
                    .and_then(|n| n.parse::<usize>().ok())
                else {
                    continue;
                };
                // Check size before reading: a page that no caller can use is
                // not worth pulling into memory.
                match entry.metadata().await {
                    Ok(m) if m.len() as usize <= max_page_bytes => {}
                    Ok(m) => {
                        tracing::warn!(
                            bytes = m.len(),
                            max = max_page_bytes,
                            "extract: rendered page too large, skipped"
                        );
                        continue;
                    }
                    Err(_) => continue,
                }
                if let Ok(bytes) = tokio::fs::read(&p).await {
                    pages.push((page_no, bytes));
                }
            }
        }
    }

    // `work` removes itself on drop, cancellation included.
    // Directory order is arbitrary — sort so callers get page order.
    pages.sort_by_key(|(n, _)| *n);
    pages.truncate(max_pages);
    pages
}

/// What a converter needs bound into its sandbox.
///
/// The runner cannot infer this from argv — a path argument may be the input,
/// an output directory, or a `-env:` URL — so callers declare it.
struct Sandbox<'a> {
    /// The document being converted. Bound read-only.
    input: &'a Path,
    /// Scratch the converter writes into (LibreOffice profile, `pdftoppm`
    /// output). `None` for converters that only write stdout.
    work: Option<&'a Path>,
}

/// Wrap a converter invocation in `bwrap`, or return it unchanged when bwrap is
/// not installed.
///
/// The threat is the document, not the binary: LibreOffice's legacy import
/// filters are its densest CVE surface and run as the kernel user, next to the
/// vault and audit databases. `--unshare-all` is also the only real answer to
/// remote-fetching document features — the profile seed covers the ones
/// LibreOffice exposes as settings, this covers the rest.
async fn sandbox_command(
    program: &str,
    args: &[OsString],
    sb: &Sandbox<'_>,
) -> (String, Vec<OsString>) {
    if !sandbox_fs::bwrap_usable().await {
        return (program.to_string(), args.to_vec());
    }

    let mut a: Vec<OsString> = Vec::with_capacity(args.len() + 48);
    // Read-only system. Only what exists: bwrap fails the whole call on a
    // missing bind source, and `/lib64` is absent on some hosts.
    // `/opt` and `/snap`: the upstream LibreOffice .deb/.rpm installs to
    // `/opt/libreofficeX.Y` and only symlinks into `/usr/local/bin`, so without
    // these the symlink resolves to nothing inside the sandbox and office
    // conversion dies on exactly the hosts that installed it from libreoffice.org.
    a.extend(sandbox_fs::ro_bind_existing(sandbox_fs::SYSTEM_RO_DIRS));
    a.extend(sandbox_fs::ro_bind_existing(&["/opt", "/snap"]));

    let sized_tmp = sandbox_fs::bwrap_supports_tmpfs_size().await;
    let mut flags = |xs: &[&str]| a.extend(xs.iter().map(OsString::from));

    // Blank out everything a converter has no business reading, then bind back
    // only what it needs. Order matters — bwrap applies arguments in sequence,
    // so a bind before its tmpfs is shadowed.
    for dir in ["/etc", "/home", "/root", "/var"] {
        flags(&["--tmpfs", dir]);
    }
    // `/tmp` is sized. A bare `--tmpfs` takes the kernel default of half of RAM,
    // and converters spool there: a 60 MiB spreadsheet whose ZIP members expand
    // a thousandfold would write that expansion into memory, which the input cap
    // and the timeout do not bound.
    //
    // `--size` needs bubblewrap >= 0.9 (Ubuntu 22.04 ships 0.6.1, which rejects
    // the whole invocation). Without it `/tmp` is unsized; the spooling
    // converters are still steered to disk because `TMPDIR` below points at the
    // work dir, and the stdout-only ones do not spool.
    if sized_tmp {
        flags(&["--size", TMPFS_BYTES, "--tmpfs", "/tmp"]);
    } else {
        flags(&["--tmpfs", "/tmp"]);
    }
    // Fontconfig's system cache, masked by the `/var` tmpfs above. Without it
    // every single invocation rescans every font on the host — seconds of CPU
    // per document, and enough on a font-heavy machine to push LibreOffice's
    // cold start into CONVERT_TIMEOUT.
    if std::path::Path::new("/var/cache/fontconfig").exists() {
        flags(&[
            "--ro-bind",
            "/var/cache/fontconfig",
            "/var/cache/fontconfig",
        ]);
    }
    // `/etc` back. An empty `/etc` is not survivable for LibreOffice: it aborts
    // during `SvtSysLocaleOptions` construction with an uncaught
    // `RuntimeException` (verified — `Fatal exception: Signal 6`) without
    // `passwd`/`fonts`/`localtime`. The shared runtime set covers those plus
    // the loader cache and alternatives symlinks every sandbox needs; the
    // distro registry overlay is LibreOffice's own and absent on some hosts.
    a.extend(sandbox_fs::ro_bind_existing(sandbox_fs::ETC_RUNTIME));
    a.extend(sandbox_fs::ro_bind_existing(&["/etc/libreoffice"]));

    let home = sb.work.unwrap_or(std::path::Path::new("/tmp"));
    let mut binds: Vec<OsString> = Vec::new();
    if let Some(work) = sb.work {
        binds.push(OsString::from("--bind"));
        binds.push(work.as_os_str().to_os_string());
        binds.push(work.as_os_str().to_os_string());
    }
    // Read-only, and after the tmpfs steps so an upload under /home or /tmp is
    // not shadowed by them.
    binds.push(OsString::from("--ro-bind"));
    binds.push(sb.input.as_os_str().to_os_string());
    binds.push(sb.input.as_os_str().to_os_string());
    a.extend(binds);

    a.extend(
        [
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-all",
            // Drop the controlling terminal: `--dev` binds the host `/dev/tty`,
            // and without a new session a converter shares the operator's
            // terminal.
            "--new-session",
            // A SIGKILLed kernel runs no destructors, so `KillGroup` never
            // fires; this is what stops bwrap and its ~250 MB `soffice.bin`
            // from surviving as orphans.
            "--die-with-parent",
            "--clearenv",
            "--setenv",
            "PATH",
            "/usr/local/bin:/usr/bin:/bin",
            "--setenv",
            "LC_ALL",
            "C.UTF-8",
            "--setenv",
            "TMPDIR",
        ]
        .iter()
        .map(OsString::from),
    );
    // Spool onto the disk-backed work bind when there is one, not the sized
    // `/tmp` tmpfs — a converter that needs more scratch than the tmpfs allows
    // should slow down, not fail.
    a.push(home.as_os_str().to_os_string());
    a.push(OsString::from("--setenv"));
    a.push(OsString::from("HOME"));
    a.push(home.as_os_str().to_os_string());
    a.push(OsString::from("--"));
    a.push(OsString::from(program));
    a.extend(args.iter().cloned());

    ("bwrap".to_string(), a)
}

/// Run an external converter, returning its stdout and whether the byte cap
/// was hit.
///
/// A missing binary, a non-zero exit and a timeout are all `None` — extraction
/// is best-effort by contract.
///
/// stdout is streamed under a hard byte cap rather than collected with
/// `Command::output()`: `pdftotext` on a PDF whose content stream expands
/// thousands-to-one emits gigabytes well inside the timeout, and buffering that
/// would take the process down.
async fn run_capture(
    program: &str,
    args: &[OsString],
    sb: &Sandbox<'_>,
) -> Option<(Vec<u8>, bool)> {
    let (program, args) = sandbox_command(program, args, sb).await;
    run_capture_limited(&program, &args, MAX_TEXT_BYTES).await
}

/// [`run_capture`] with an explicit stdout cap and no sandbox, so tests can
/// exercise the overrun path without allocating the production limit.
async fn run_capture_limited(
    program: &str,
    args: &[OsString],
    max_bytes: usize,
) -> Option<(Vec<u8>, bool)> {
    use tokio::io::AsyncReadExt;

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Converters need no inherited environment, and the kernel's may hold
    // provider API keys. HOME is always redirected: without it glibc falls back
    // to `getpwuid()` and poppler writes the operator's real
    // `~/.cache/fontconfig`.
    cmd.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .env(
            "HOME",
            args.iter()
                .find_map(office_profile_home)
                .unwrap_or_else(std::env::temp_dir),
        );

    // Own process group: `kill_on_drop` only signals the direct child, and
    // `soffice` is a shell wrapper that execs `soffice.bin` via `oosplash`.
    // Without this, a timed-out conversion leaves the real worker running.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(program, error = %e, "extract: converter unavailable");
            return None;
        }
    };
    // Armed from here on. Every exit that does not reap the child — timeout,
    // overrun, I/O error, and a cancelled caller — must take the whole group
    // with it, or a dropped future leaves a 250 MB `soffice.bin` running while
    // its semaphore permit is already back in the pool.
    let mut guard = KillGroup(child.id());
    let mut stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;

    // stderr is drained by its own task rather than after stdout: a converter
    // that fills the 64 KiB pipe blocks in `write(2)`, never closes stdout, and
    // a stdout-first read would then wait out the whole timeout on a document
    // that converted fine.
    let stderr_task = tokio::spawn(drain_capped(stderr));

    let collect = async {
        let mut out = Vec::new();
        // One extra byte so "exactly at the cap" is distinguishable from
        // "more was coming".
        let mut capped = (&mut stdout).take(max_bytes as u64 + 1);
        capped.read_to_end(&mut out).await?;

        if out.len() > max_bytes {
            // Overrun. Do not wait on the child: it is blocked writing into a
            // pipe nobody is draining, and waiting would stall for the full
            // timeout on every oversized document.
            return Ok::<_, std::io::Error>((out, None));
        }

        let status = child.wait().await?;
        Ok((out, Some(status)))
    };

    match tokio::time::timeout(CONVERT_TIMEOUT, collect).await {
        Ok(Ok((out, None))) => {
            tracing::warn!(
                program,
                max_bytes,
                "extract: converter output exceeded cap, truncated"
            );
            Some((out, true))
        }
        Ok(Ok((out, Some(status)))) => {
            if status.success() {
                // Reaped, so the pid can be recycled — signalling it now could
                // hit an unrelated group.
                guard.disarm();
                Some((out, false))
            } else {
                // Bounded, and the guard stays armed until it returns. The
                // stderr pipe closes only when every holder of the write end
                // does, and a `soffice` wrapper can exit non-zero while leaving
                // a `soffice.bin` holding fd 2 — an unbounded await there hangs
                // the caller forever with the office permit still held.
                let drained = tokio::time::timeout(STDERR_DRAIN_GRACE, stderr_task).await;
                // Disarm only when stderr reached EOF. If it did not, something
                // is still holding the write end after the child was reaped —
                // which is precisely the survivor the group kill exists for.
                let err = match drained {
                    Ok(joined) => {
                        guard.disarm();
                        joined.unwrap_or_default()
                    }
                    Err(_) => {
                        tracing::warn!(
                            program,
                            "extract: converter exited but stderr stayed open; killing its group"
                        );
                        Vec::new()
                    }
                };
                tracing::warn!(
                    program,
                    code = status.code(),
                    stderr = %String::from_utf8_lossy(&err).chars().take(200).collect::<String>(),
                    "extract: converter exited non-zero"
                );
                None
            }
        }
        Ok(Err(e)) => {
            tracing::warn!(program, error = %e, "extract: converter I/O failed");
            None
        }
        Err(_) => {
            tracing::warn!(
                program,
                timeout_s = CONVERT_TIMEOUT.as_secs(),
                "extract: converter timed out"
            );
            None
        }
    }
}

/// Read a converter's stderr to EOF, keeping only the head.
///
/// Reading to EOF is the point: stopping early leaves the child blocked on a
/// full pipe. The head is all any log line needs.
async fn drain_capped(mut stderr: tokio::process::ChildStderr) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    const KEEP: usize = 8 * 1024;
    let mut kept = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = stderr.read(&mut buf).await {
        if n == 0 {
            break;
        }
        if kept.len() < KEEP {
            kept.extend_from_slice(&buf[..n.min(KEEP - kept.len())]);
        }
    }
    kept
}

/// SIGKILLs a converter's process group on drop unless disarmed.
///
/// A `Drop` and not a call at each exit: the caller's future can be dropped at
/// any await point (an axum handler on client disconnect), and `kill_on_drop`
/// reaps only the direct child — `soffice`'s real worker is a grandchild.
struct KillGroup(Option<u32>);

impl KillGroup {
    /// Call once the child has been reaped: its pid is then free for reuse, and
    /// signalling a recycled pid would hit an unrelated process group.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for KillGroup {
    fn drop(&mut self) {
        kill_process_group(self.0);
    }
}

/// Render a path as a `file://` URL, or `None` when it cannot be one safely.
///
/// Deliberately conservative: rather than percent-encode, this rejects anything
/// outside a plain-ASCII safe set. The only input is a temp dir we created
/// under `$TMPDIR`, so refusing is cheap, and a half-right encoding here fails
/// *open* — LibreOffice would quietly use an unseeded profile.
fn file_url(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | '+' | '~'));
    if !safe || !s.starts_with('/') {
        return None;
    }
    Some(format!("file://{s}"))
}

/// LibreOffice's `-env:UserInstallation=file://<dir>/profile` argument names the
/// only writable directory the call needs, so its parent stands in for `HOME`.
/// The profile directory itself is created by [`seed_office_profile`].
fn office_profile_home(arg: &OsString) -> Option<PathBuf> {
    let s = arg.to_str()?;
    let dir = s.strip_prefix("-env:UserInstallation=file://")?;
    Some(PathBuf::from(dir))
}

/// SIGKILL the converter's whole process group after a timeout or an overrun.
///
/// `process_group(0)` at spawn makes the child a group leader, so its pid is
/// also its pgid. Must be a direct `killpg`: shelling out to `/usr/bin/kill`
/// with a negative pid is parsed as an option by util-linux and signals the
/// *caller's* group instead — i.e. it kills the kernel.
#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    // SAFETY: killpg with a pgid this process created via `process_group(0)`;
    // the call has no memory effects and a failure (already-exited group) is
    // reported through the return value we deliberately ignore.
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {}

/// Read a file as UTF-8 up to [`MAX_TEXT_BYTES`], replacing invalid sequences
/// rather than failing — a latin-1 CSV is still far more useful to an agent
/// than an error.
async fn read_capped_text(path: &Path) -> Option<String> {
    // One byte past the cap so the read itself says whether there was more.
    let bytes = read_head(path, MAX_TEXT_BYTES + 1).await?;
    // Truncation is decided by the read, never by a second `stat`: a sysfs-style
    // pseudo-file reports 4096 bytes and yields 7, and trusting the stat marked
    // that complete 7-byte value as "truncated at 5 MiB". The stat is only
    // consulted for the *number* to report once the read has established that
    // the file really is longer.
    let capped = bytes.len() > MAX_TEXT_BYTES;
    let total = if capped { file_len(path).await } else { None };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(if capped {
        truncate_text(text, total)
    } else {
        text
    })
}

/// Read a file whose MIME claims it is text.
///
/// Looser than [`sniff_text`] — a latin-1 log is worth handing over even though
/// it is not valid UTF-8 — but not unconditional: the MIME is supplied by
/// whoever uploaded the file, and a NUL says the claim is false.
async fn read_declared_text(path: &Path) -> Option<String> {
    let head = read_head(path, SNIFF_BYTES).await?;
    if head.contains(&0) {
        return None;
    }
    read_capped_text(path).await
}

/// Byte length of a file, or `None` if it cannot be stat'd.
async fn file_len(path: &Path) -> Option<u64> {
    tokio::fs::metadata(path).await.ok().map(|m| m.len())
}

/// Treat an undeclared file as text when its head is valid UTF-8 and not
/// control-character dense. Covers the common case of source code, logs and
/// CSVs uploaded as `application/octet-stream`.
async fn sniff_text(path: &Path) -> Option<String> {
    let bytes = read_head(path, SNIFF_BYTES).await?;
    if !looks_texty(&bytes) {
        return None;
    }
    read_capped_text(path).await
}

/// Whether a byte sample reads as text: valid UTF-8, no NUL, and not
/// control-character dense.
fn looks_texty(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let head = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        // A truncated multi-byte char at the sample boundary is expected; a
        // real encoding error is not.
        Err(e) if e.error_len().is_none() => match std::str::from_utf8(&bytes[..e.valid_up_to()]) {
            Ok(s) => s,
            Err(_) => return false,
        },
        Err(_) => return false,
    };

    let controls = head
        .chars()
        .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        .count();
    !head.contains('\0')
        && controls as f32 / head.chars().count().max(1) as f32 <= MAX_CONTROL_RATIO
}

/// Read at most `limit` bytes from the head of a file.
async fn read_head(path: &Path, limit: usize) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "extract: cannot open file");
            return None;
        }
    };
    let mut buf = Vec::with_capacity(limit.min(8 * 1024));
    if let Err(e) = file.take(limit as u64).read_to_end(&mut buf).await {
        tracing::warn!(path = %path.display(), error = %e, "extract: read failed");
        return None;
    }
    Some(buf)
}

/// Cap extracted text, cutting on a char boundary and marking the cut.
///
/// `real_total` is the source's true byte length when the caller knows it. It
/// is a separate argument because `text` may already be a prefix — every read
/// here caps itself — so `text.len()` understates the document by any amount.
fn truncate_text(text: String, real_total: Option<u64>) -> String {
    let already_short = match real_total {
        Some(t) => t <= text.len() as u64,
        None => true,
    };
    if text.len() <= MAX_TEXT_BYTES && already_short {
        return text;
    }
    let cut = text
        .char_indices()
        .take_while(|(i, _)| *i < MAX_TEXT_BYTES)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let mut out = text[..cut].to_string();
    out.push_str(&truncation_marker(MAX_TEXT_BYTES, real_total));
    out
}

/// The line appended to any truncated extraction.
///
/// Without a known total it says only that the file continues: `text.len()` at
/// that point is the cap, and printing it as the total would tell the model it
/// has the whole document.
fn truncation_marker(cap: usize, real_total: Option<u64>) -> String {
    let mib = cap as f32 / (1024.0 * 1024.0);
    match real_total {
        Some(t) => format!("\n[... truncated at {mib:.0} MiB — {t} bytes total]"),
        None => format!("\n[... truncated at {mib:.0} MiB — the file continues past this point]"),
    }
}

/// Build a minimal single-page PDF containing `text`, so tests in this crate
/// and in `agentos-web` need no binary fixture checked into the repo.
#[doc(hidden)]
pub fn minimal_pdf_fixture(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];

    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }

    let xref_at = pdf.len();
    pdf.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for off in &offsets {
        pdf.push_str(&format!("{off:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
        objects.len() + 1
    ));
    pdf.into_bytes()
}

/// Whitespace-only extraction means the converter produced nothing usable.
fn non_empty(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have(program: &str) -> bool {
        std::process::Command::new(program)
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    #[test]
    fn text_mimes_recognized() {
        assert!(is_text_mime("text/plain"));
        assert!(is_text_mime("application/json"));
        assert!(is_text_mime("text/markdown"));
        assert!(is_text_mime("image/svg+xml"));
        assert!(!is_text_mime("application/pdf"));
        assert!(!is_text_mime("image/png"));
        // A .docx MIME contains "xml" but the bytes are a ZIP archive.
        assert!(!is_text_mime(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        ));
        assert!(!is_text_mime(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        ));
    }

    #[test]
    fn converters_selected_by_mime_or_extension() {
        assert_eq!(
            converter_for("application/pdf", ""),
            Some(Converter::PdfText)
        );
        assert_eq!(
            converter_for("application/octet-stream", "pdf"),
            Some(Converter::PdfText)
        );
        assert_eq!(
            converter_for(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                ""
            ),
            Some(Converter::Office { spreadsheet: false })
        );
        assert_eq!(
            converter_for("application/octet-stream", "xlsx"),
            Some(Converter::Office { spreadsheet: true })
        );
        assert_eq!(converter_for("text/html", ""), Some(Converter::Html));
        assert_eq!(converter_for("text/plain", "txt"), None);
        // Windows sends CSV as an Excel MIME; round-tripping it through
        // LibreOffice drops leading zeros and relocalizes dates.
        assert_eq!(converter_for("application/vnd.ms-excel", "csv"), None);
        // `.key` in the wild is a PEM secret far more often than a Keynote deck.
        assert_eq!(converter_for("application/octet-stream", "key"), None);
    }

    #[tokio::test]
    async fn relative_paths_are_refused() {
        assert!(
            read_as_text(std::path::Path::new("relative.txt"), "text/plain")
                .await
                .is_none()
        );
    }

    /// A PNG that happens to be named `.htm` must not be dragged onto the HTML
    /// branch by its extension.
    #[tokio::test]
    async fn image_mime_wins_over_html_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("photo.htm");
        tokio::fs::write(&p, [0x89u8, b'P', b'N', b'G', 0, 1, 2, 3])
            .await
            .unwrap();
        assert!(read_as_text(&p, "image/png").await.is_none());
    }

    /// Markup that renders to nothing (an SPA shell) is still worth showing.
    #[tokio::test]
    async fn html_rendering_to_nothing_falls_back_to_raw_markup() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("app.html");
        tokio::fs::write(&p, "<div id=\"root\"></div>")
            .await
            .unwrap();
        let out = read_as_text(&p, "text/html").await;
        assert!(
            out.as_deref().is_some_and(|s| s.contains("id=\"root\"")),
            "got {out:?}"
        );
    }

    /// A converter that never stops writing must be cut off at the cap and
    /// killed, not buffered until the host runs out of memory.
    #[tokio::test]
    async fn endless_converter_output_is_capped_and_killed() {
        if !have("yes") {
            eprintln!("skipping: yes not installed");
            return;
        }
        let started = std::time::Instant::now();
        let out = run_capture_limited("yes", &[OsString::from("spam")], 4096).await;
        // One byte past the cap, and flagged: that extra byte is what tells the
        // caller to mark the text as a prefix rather than silently hand over
        // exactly-the-cap bytes as if they were the whole output.
        assert_eq!(
            out.as_ref().map(|(o, hit)| (o.len(), *hit)),
            Some((4097, true))
        );
        // Must return on the cap, not on CONVERT_TIMEOUT.
        assert!(
            started.elapsed() < CONVERT_TIMEOUT,
            "capped read waited for the timeout"
        );
    }

    #[test]
    fn texty_detection_rejects_binary() {
        assert!(looks_texty(b"fn main() {}\n"));
        assert!(!looks_texty(&[0u8, 1, 2, 3, 0, 255]));
        assert!(!looks_texty(b""));
    }

    #[tokio::test]
    async fn declared_text_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        tokio::fs::write(&p, "hello world").await.unwrap();
        assert_eq!(
            read_as_text(&p, "text/plain").await.as_deref(),
            Some("hello world")
        );
    }

    #[tokio::test]
    async fn utf8_source_with_binary_mime_is_sniffed_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("main.rs");
        tokio::fs::write(&p, "fn main() {\n    println!(\"hi\");\n}\n")
            .await
            .unwrap();
        let out = read_as_text(&p, "application/octet-stream").await;
        assert!(out.is_some_and(|s| s.contains("fn main()")));
    }

    #[tokio::test]
    async fn random_bytes_stay_binary() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blob.bin");
        let bytes: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        tokio::fs::write(&p, bytes).await.unwrap();
        assert!(read_as_text(&p, "application/octet-stream").await.is_none());
    }

    #[tokio::test]
    async fn images_are_declined_even_when_ascii() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fake.png");
        tokio::fs::write(&p, "not really a png").await.unwrap();
        assert!(read_as_text(&p, "image/png").await.is_none());
    }

    /// SVG is markup, not raster — it must stay on the text path.
    #[tokio::test]
    async fn svg_is_inlined_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("logo.svg");
        tokio::fs::write(&p, "<svg><title>Logo</title></svg>")
            .await
            .unwrap();
        let out = read_as_text(&p, "image/svg+xml").await;
        assert!(out.is_some_and(|s| s.contains("<title>Logo</title>")));
    }

    #[tokio::test]
    async fn html_is_stripped_to_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("page.html");
        tokio::fs::write(
            &p,
            "<html><body><h1>Title</h1><p>Body text</p></body></html>",
        )
        .await
        .unwrap();
        let out = read_as_text(&p, "text/html").await.unwrap();
        assert!(out.contains("Title") && out.contains("Body text"));
        assert!(!out.contains("<h1>"));
    }

    #[test]
    fn long_text_is_truncated_with_marker() {
        let big = "a".repeat(MAX_TEXT_BYTES + 1024);
        let out = truncate_text(big, None);
        assert!(out.contains("[... truncated at 5 MiB"));
        assert!(out.len() < MAX_TEXT_BYTES + 200);
    }

    /// Text that fits the cap but came from a longer file is still a prefix.
    /// Without the real total the marker never fires, and the model is told a
    /// 5 MiB slice of a 500 MiB log is the whole log.
    #[test]
    fn capped_read_of_a_longer_file_is_marked() {
        let at_cap = "a".repeat(MAX_TEXT_BYTES);
        let out = truncate_text(at_cap, Some(500 * 1024 * 1024));
        assert!(
            out.contains("524288000 bytes total"),
            "got tail {:?}",
            &out[out.len() - 80..]
        );
    }

    /// With no known total the marker must not print one — the text length at
    /// that point is the cap, and printing it reads as "this is the whole file".
    #[test]
    fn unknown_total_does_not_invent_one() {
        let over = "a".repeat(MAX_TEXT_BYTES + 10);
        let out = truncate_text(over, None);
        assert!(out.contains("the file continues past this point"));
        assert!(!out.contains("bytes total"));
    }

    #[tokio::test]
    async fn pdf_text_layer_is_extracted() {
        if !have("pdftotext") {
            eprintln!("skipping: pdftotext not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.pdf");
        tokio::fs::write(&p, minimal_pdf_fixture("Hello from PDF"))
            .await
            .unwrap();
        let out = read_as_text(&p, "application/pdf").await;
        assert!(
            out.as_deref().is_some_and(|s| s.contains("Hello from PDF")),
            "got {out:?}"
        );
    }

    /// Round-trips through LibreOffice: build a `.docx` from plain text, then
    /// convert it back. Slow (~10s cold start) but it is the only check that
    /// the `soffice` argv — private profile, filter string, outdir — is right.
    #[tokio::test]
    async fn office_document_is_converted_to_text() {
        if !have("soffice") {
            eprintln!("skipping: soffice not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        tokio::fs::write(&src, "Quarterly revenue was 42 million.\n")
            .await
            .unwrap();

        let profile = dir.path().join("mkprofile");
        let made = run_capture(
            "soffice",
            &[
                OsString::from("--headless"),
                OsString::from("--norestore"),
                OsString::from(format!(
                    "-env:UserInstallation=file://{}",
                    profile.display()
                )),
                OsString::from("--convert-to"),
                OsString::from("docx"),
                OsString::from("--outdir"),
                dir.path().as_os_str().to_os_string(),
                src.as_os_str().to_os_string(),
            ],
            // The fixture build writes its output next to the source, so the
            // whole tempdir is the writable surface here.
            &Sandbox {
                input: &src,
                work: Some(dir.path()),
            },
        )
        .await;
        let docx = dir.path().join("src.docx");
        // Asserted, not skipped: this same call is how `office_to_text` invokes
        // LibreOffice, so a sandbox that breaks it must fail the suite. Skipping
        // here once hid exactly that — `bwrap` with a `tmpfs` over `/etc` makes
        // `soffice` abort, and the test still reported green.
        assert!(
            made.is_some() && docx.exists(),
            "soffice is installed but produced no output — the sandbox bind set is probably wrong"
        );

        let out = read_as_text(
            &docx,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .await;
        assert!(
            out.as_deref().is_some_and(|s| s.contains("42 million")),
            "got {out:?}"
        );
    }

    #[tokio::test]
    async fn rasterize_declines_non_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("not.pdf");
        tokio::fs::write(&p, "definitely not a pdf").await.unwrap();
        assert!(rasterize_pdf(&p, 2, 5 * 1024 * 1024).await.is_empty());
    }

    #[tokio::test]
    async fn rasterize_renders_png_pages() {
        if !have("pdftoppm") {
            eprintln!("skipping: pdftoppm not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.pdf");
        tokio::fs::write(&p, minimal_pdf_fixture("page one"))
            .await
            .unwrap();
        let pages = rasterize_pdf(&p, 3, 5 * 1024 * 1024).await;
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].0, 1, "page number comes from the file name");
        assert_eq!(&pages[0].1[..4], b"\x89PNG");
    }

    /// A converter selected by extension must not skip the input size gate just
    /// because the MIME is media-ish. `image/svg+xml` is both text and media,
    /// and gating on the media family let an oversized upload through.
    #[tokio::test]
    async fn oversized_input_is_never_converted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("payload.docx");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_CONVERT_INPUT_BYTES + 1).unwrap();
        drop(f);
        // Sparse, so it is all NULs: too big to convert, and the sniff refuses
        // it as binary. Either way it must not reach LibreOffice.
        let started = std::time::Instant::now();
        assert!(read_as_text(&p, "image/svg+xml").await.is_none());
        assert!(
            started.elapsed() < CONVERT_TIMEOUT,
            "oversized input reached a converter"
        );
    }

    /// MIME outranks extension: a PDF served as `report.htm` used to be handed
    /// to html2text, which declines, losing a file `pdftotext` reads.
    #[test]
    fn mime_outranks_extension() {
        assert_eq!(
            converter_for("application/pdf", "htm"),
            Some(Converter::PdfText)
        );
        assert_eq!(converter_for("text/html", "pdf"), Some(Converter::Html));
    }
}
