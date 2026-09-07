//! Image resolution and MIME validation for multimodal LLM requests.
//!
//! `prepare_for_inference` is the single async entry point that walks a
//! `ContextWindow`, fetches/decodes/validates every `ContentPart::Image`, and
//! returns a window where every Image part has been either:
//!   - resolved to `ImageSource::Base64` (vision-supported, validated, capped), or
//!   - replaced with a `ContentPart::Text` stub (capability gate, per-turn cap,
//!     fetch failure, MIME spoof, SSRF block, etc.).
//!
//! After preparation, the per-provider sync helpers (`anthropic_blocks_for_entry`,
//! `openai_user_content_value`, `gemini_user_parts`) only ever see Base64 source
//! and can stay synchronous.

use agentos_types::{AgentOSError, ContentPart, ContextEntry, ContextWindow, ImageSource};
use async_trait::async_trait;
use base64::Engine;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Maximum decoded bytes per inline image (v1 cap).
pub const MAX_INLINE_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Maximum image parts allowed per `ContextEntry` after preparation. Images
/// beyond this become text stubs to bound prompt size and provider cost.
pub const MAX_IMAGES_PER_TURN: usize = 5;

/// Lazily resolves [`ImageSource::FileRef`] to `(mime, base64)` for adapters.
///
/// Implementations may block (FS reads); callers must invoke this from a
/// `spawn_blocking` context. `prepare_for_inference` does this.
pub trait ImageResolver: Send + Sync {
    fn resolve_base64(&self, file_id: &str) -> Result<(String, String), AgentOSError>;

    /// Optional human-readable filename for stub messages. Default: `file_id`.
    fn resolve_filename(&self, file_id: &str) -> Option<String> {
        let _ = file_id;
        None
    }
}

/// Default when no file store is wired (CLI / tests).
#[derive(Debug, Default)]
pub struct NoopImageResolver;

impl ImageResolver for NoopImageResolver {
    fn resolve_base64(&self, file_id: &str) -> Result<(String, String), AgentOSError> {
        Err(AgentOSError::LLMError {
            provider: "image-resolver".to_string(),
            reason: format!("file resolver not configured (file_id={file_id})"),
        })
    }
}

pub fn is_supported_image_mime(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}

fn image_stub_note(filename_hint: &str, mime: &str, note: &str) -> String {
    format!("[Image: {filename_hint}, mime={mime} — {note}]")
}

/// When the adapter cannot emit native images, replace with a short text stub (no silent drop).
pub fn image_fallback_stub(filename_hint: &str, mime: &str) -> String {
    image_stub_note(
        filename_hint,
        mime,
        "model does not support vision; attach with a vision-capable model for pixels",
    )
}

/// Inline copy of the SSRF guard maintained alongside `agentos-tools::ssrf`.
///
/// Kept inline here to avoid a heavy dep on `agentos-tools` from `agentos-llm`.
/// If you change one, change the other — both must reject the cloud-metadata
/// endpoint (169.254.169.254).
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            ipv4.is_loopback()
                || ipv4.is_unspecified()
                || ipv4.is_private()
                || ipv4.is_link_local()
                || ipv4.is_multicast()
                || {
                    let o = ipv4.octets();
                    o[0] == 100 && o[1] >= 64 && o[1] < 128 // 100.64/10 CGN
                }
        }
        IpAddr::V6(ipv6) => {
            if let Some(ipv4) = ipv6.to_ipv4_mapped() {
                return is_private_ip(&IpAddr::V4(ipv4));
            }
            ipv6.is_loopback()
                || ipv6.is_unspecified()
                || ipv6.is_multicast()
                || (ipv6.segments()[0] & 0xfe00) == 0xfc00
                || (ipv6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Returns Err if `host` resolves to any IP in a private range. Best-effort:
/// resolution failure also returns Err to fail closed.
fn check_host_ssrf(host: &str) -> Result<(), AgentOSError> {
    use std::net::ToSocketAddrs;
    // Some hosts are literal IPs; parse first to bypass DNS.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(&ip) {
            return Err(AgentOSError::SchemaValidation(format!(
                "image URL host resolves to blocked range: {ip}"
            )));
        }
        return Ok(());
    }
    let addrs = (host, 0u16).to_socket_addrs().map_err(|e| {
        AgentOSError::SchemaValidation(format!("could not resolve image URL host {host}: {e}"))
    })?;
    let mut any = false;
    for sa in addrs {
        any = true;
        if is_private_ip(&sa.ip()) {
            return Err(AgentOSError::SchemaValidation(format!(
                "image URL host resolves to blocked range: {}",
                sa.ip()
            )));
        }
    }
    if !any {
        return Err(AgentOSError::SchemaValidation(format!(
            "image URL host resolves to no addresses: {host}"
        )));
    }
    Ok(())
}

fn sniff_mime(hint: &str, url_or_name: &str, bytes: &[u8]) -> String {
    if is_supported_image_mime(hint) {
        // Confirm hint by matching magic bytes; if it disagrees, prefer sniff.
        if let Some(sniffed) = magic_bytes_mime(bytes) {
            return sniffed.to_string();
        }
        return hint.to_string();
    }
    let lc = url_or_name.to_ascii_lowercase();
    let ext_mime = if lc.ends_with(".png") {
        Some("image/png")
    } else if lc.ends_with(".jpg") || lc.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lc.ends_with(".webp") {
        Some("image/webp")
    } else if lc.ends_with(".gif") {
        Some("image/gif")
    } else {
        None
    };
    if let Some(m) = ext_mime {
        return m.to_string();
    }
    if let Some(m) = magic_bytes_mime(bytes) {
        return m.to_string();
    }
    hint.to_string()
}

fn magic_bytes_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] {
        return Some("image/png");
    }
    if bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff] {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

fn parse_data_uri(url: &str) -> Result<(String, String), AgentOSError> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| AgentOSError::SchemaValidation("invalid data URI".into()))?;
    let (meta, b64) = rest
        .split_once(',')
        .ok_or_else(|| AgentOSError::SchemaValidation("invalid data URI (no comma)".into()))?;
    if !meta
        .split(';')
        .any(|s| s.trim().eq_ignore_ascii_case("base64"))
    {
        return Err(AgentOSError::SchemaValidation(
            "data URI must be ;base64 encoded".into(),
        ));
    }
    let mime = meta
        .split(';')
        .next()
        .unwrap_or("image/png")
        .trim()
        .to_ascii_lowercase();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| AgentOSError::SchemaValidation(format!("invalid data URI base64: {e}")))?;
    if bytes.len() > MAX_INLINE_IMAGE_BYTES {
        return Err(AgentOSError::SchemaValidation(format!(
            "image exceeds max inline size ({} bytes)",
            MAX_INLINE_IMAGE_BYTES
        )));
    }
    let sniffed = sniff_mime(&mime, "", &bytes);
    if !is_supported_image_mime(&sniffed) {
        return Err(AgentOSError::SchemaValidation(format!(
            "unsupported image MIME in data URI: {sniffed}"
        )));
    }
    Ok((sniffed, b64.trim().to_string()))
}

/// Validate a Base64 source declared with `mime`. Requires magic-byte
/// confirmation — bytes that don't match any known image signature are rejected
/// even if the declared MIME is valid (prevents MIME spoofing with garbage data).
/// Returns `(mime, b64)` — mime replaced with magic-sniffed value when they differ.
fn validate_base64_source(mime: &str, data: &str) -> Result<(String, String), AgentOSError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|e| AgentOSError::SchemaValidation(format!("invalid base64 image data: {e}")))?;
    if bytes.len() > MAX_INLINE_IMAGE_BYTES {
        return Err(AgentOSError::SchemaValidation(format!(
            "image exceeds max inline size ({} bytes)",
            MAX_INLINE_IMAGE_BYTES
        )));
    }
    // Require magic-byte confirmation — hint alone is not sufficient.
    let confirmed = magic_bytes_mime(&bytes).ok_or_else(|| {
        AgentOSError::SchemaValidation(format!(
            "image bytes do not match any supported format (declared={mime})"
        ))
    })?;
    Ok((confirmed.to_string(), data.trim().to_string()))
}

/// Shared HTTP client for image fetches with redirects DISABLED. We follow
/// redirects manually so that `check_host_ssrf` runs on every hop — reqwest's
/// default auto-follow would let a public URL 30x-redirect to an internal
/// address (e.g. 169.254.169.254 cloud metadata) after the initial host passed
/// the SSRF check.
static IMAGE_FETCH_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn image_fetch_client() -> &'static reqwest::Client {
    IMAGE_FETCH_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Max redirect hops to follow when fetching an image URL.
const MAX_IMAGE_REDIRECTS: usize = 5;

/// Follow redirects manually, running `check_host_ssrf` on every hop, and
/// return the first non-redirect response. `current` is updated in place to the
/// final URL so the caller can use it for MIME sniffing.
async fn loop_with_ssrf_checks(
    client: &reqwest::Client,
    current: &mut url::Url,
) -> Result<reqwest::Response, AgentOSError> {
    for _hop in 0..=MAX_IMAGE_REDIRECTS {
        match current.scheme() {
            "http" | "https" => {}
            s => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "unsupported image URL scheme: {s}"
                )))
            }
        }
        let host = current
            .host_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("image URL missing host".into()))?
            .to_string();
        check_host_ssrf(&host)?;

        let resp =
            client
                .get(current.clone())
                .send()
                .await
                .map_err(|e| AgentOSError::LLMError {
                    provider: "image-fetch".into(),
                    reason: format!("failed to fetch image URL: {e}"),
                })?;

        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| AgentOSError::LLMError {
                    provider: "image-fetch".into(),
                    reason: "redirect response missing Location header".into(),
                })?;
            // Resolve relative redirects against the current URL.
            let next = current.join(location).map_err(|e| {
                AgentOSError::SchemaValidation(format!("invalid redirect URL: {e}"))
            })?;
            *current = next;
            continue;
        }
        return Ok(resp);
    }
    Err(AgentOSError::LLMError {
        provider: "image-fetch".into(),
        reason: format!("too many redirects (limit: {MAX_IMAGE_REDIRECTS})"),
    })
}

async fn fetch_url_async(
    _client: &reqwest::Client,
    url: &str,
    declared_mime: &str,
) -> Result<(String, String), AgentOSError> {
    let client = image_fetch_client();
    let mut current = url::Url::parse(url)
        .map_err(|e| AgentOSError::SchemaValidation(format!("invalid image URL: {e}")))?;

    // Manual redirect loop: validate the host of EVERY hop before connecting.
    let resp = loop_with_ssrf_checks(client, &mut current).await?;

    let url = current.as_str().to_string();
    let url = url.as_str();
    if !resp.status().is_success() {
        return Err(AgentOSError::LLMError {
            provider: "image-fetch".into(),
            reason: format!("HTTP {}", resp.status()),
        });
    }
    // Cap response size: if Content-Length advertises > cap, reject early.
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_INLINE_IMAGE_BYTES {
            return Err(AgentOSError::SchemaValidation(format!(
                "image Content-Length exceeds cap ({} bytes)",
                len
            )));
        }
    }
    let bytes = resp.bytes().await.map_err(|e| AgentOSError::LLMError {
        provider: "image-fetch".into(),
        reason: e.to_string(),
    })?;
    if bytes.len() > MAX_INLINE_IMAGE_BYTES {
        return Err(AgentOSError::SchemaValidation(format!(
            "image exceeds max inline size ({} bytes)",
            MAX_INLINE_IMAGE_BYTES
        )));
    }
    let mime = sniff_mime(&declared_mime.to_ascii_lowercase(), url, &bytes);
    if !is_supported_image_mime(&mime) {
        return Err(AgentOSError::SchemaValidation(format!(
            "unsupported image MIME after fetch: {mime}"
        )));
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok((mime, b64))
}

#[async_trait]
trait ImageResolverAsync {
    async fn resolve_base64_async(&self, file_id: &str) -> Result<(String, String), AgentOSError>;
}

#[async_trait]
impl<R: ImageResolver + ?Sized + 'static> ImageResolverAsync for Arc<R> {
    async fn resolve_base64_async(&self, file_id: &str) -> Result<(String, String), AgentOSError> {
        let resolver = self.clone();
        let id = file_id.to_string();
        tokio::task::spawn_blocking(move || resolver.resolve_base64(&id))
            .await
            .map_err(|e| AgentOSError::LLMError {
                provider: "image-resolver".into(),
                reason: format!("resolver task panicked: {e}"),
            })?
    }
}

fn warn_once_unsupported() {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "image attached to entry but adapter does not support vision; emitting text stub"
        );
    }
}

/// Walk the context window, validate every Image part, and return a context
/// where every remaining Image part is `ImageSource::Base64`. All other Image
/// parts have been replaced with `ContentPart::Text` stubs.
///
/// This is the single async entry point for image resolution. Callers can then
/// invoke per-provider sync helpers safely.
pub async fn prepare_for_inference(
    ctx: &ContextWindow,
    supports_images: bool,
    resolver: Arc<dyn ImageResolver>,
    http: &reqwest::Client,
) -> ContextWindow {
    let mut out = ctx.clone();
    let mut total_images = 0usize;
    for entry in out.entries.iter_mut() {
        for part in entry.parts.iter_mut() {
            let ContentPart::Image { mime, source } = part else {
                continue;
            };
            // Capability gate.
            if !supports_images {
                warn_once_unsupported();
                let hint = filename_hint(source, &resolver);
                *part = ContentPart::Text {
                    text: image_fallback_stub(&hint, mime),
                };
                continue;
            }
            // Per-turn cap.
            if total_images >= MAX_IMAGES_PER_TURN {
                let hint = filename_hint(source, &resolver);
                *part = ContentPart::Text {
                    text: image_stub_note(
                        &hint,
                        mime,
                        &format!("exceeded per-turn limit ({MAX_IMAGES_PER_TURN}); dropped"),
                    ),
                };
                continue;
            }
            total_images += 1;

            let result = match source.clone() {
                ImageSource::Base64 { data } => validate_base64_source(mime, &data),
                ImageSource::Url { url } => {
                    if url.starts_with("data:") {
                        parse_data_uri(&url)
                    } else {
                        fetch_url_async(http, &url, mime).await
                    }
                }
                ImageSource::FileRef { file_id } => resolver.resolve_base64_async(&file_id).await,
            };
            match result {
                Ok((m, b64)) => {
                    *mime = m;
                    *source = ImageSource::Base64 { data: b64 };
                }
                Err(e) => {
                    let hint = filename_hint(source, &resolver);
                    tracing::warn!(error = %e, "image resolution failed; emitting stub");
                    *part = ContentPart::Text {
                        text: image_stub_note(&hint, mime, &e.to_string()),
                    };
                }
            }
        }
    }
    out
}

fn filename_hint(source: &ImageSource, resolver: &Arc<dyn ImageResolver>) -> String {
    match source {
        ImageSource::FileRef { file_id } => resolver
            .resolve_filename(file_id)
            .unwrap_or_else(|| file_id.clone()),
        ImageSource::Url { url } => {
            // Strip query string + path tail.
            url.split('?')
                .next()
                .and_then(|s| s.rsplit('/').next())
                .filter(|s| !s.is_empty())
                .unwrap_or("attachment")
                .to_string()
        }
        ImageSource::Base64 { .. } => "attachment".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Per-provider sync formatters. Assume `prepare_for_inference` has already run,
// so every Image part is `ImageSource::Base64`. Image parts in any other state
// are treated as a fatal-but-recoverable bug — emit text stub.
// ---------------------------------------------------------------------------

/// Resolve image bytes assuming Base64 source (post-`prepare_for_inference`).
fn extract_base64(source: &ImageSource, mime: &str) -> Result<(String, String), AgentOSError> {
    match source {
        ImageSource::Base64 { data } => Ok((mime.to_string(), data.clone())),
        _ => Err(AgentOSError::LLMError {
            provider: "media".into(),
            reason: "image not pre-resolved (call prepare_for_inference first)".into(),
        }),
    }
}

/// Anthropic content blocks for one user-role entry.
pub fn anthropic_blocks_for_entry(
    entry: &ContextEntry,
    supports_images: bool,
    _resolver: &Arc<dyn ImageResolver>,
) -> Result<Vec<serde_json::Value>, AgentOSError> {
    let mut blocks = Vec::new();
    for part in &entry.parts {
        match part {
            ContentPart::Text { text } if !text.is_empty() => {
                blocks.push(serde_json::json!({"type": "text", "text": text}));
            }
            ContentPart::Text { .. } => {}
            ContentPart::Image { mime, source } => {
                if !supports_images {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": image_fallback_stub("attachment", mime),
                    }));
                    continue;
                }
                match extract_base64(source, mime) {
                    Ok((m, b64)) => {
                        blocks.push(serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": m,
                                "data": b64,
                            }
                        }));
                    }
                    Err(e) => {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": image_stub_note("attachment", mime, &e.to_string()),
                        }));
                    }
                }
            }
        }
    }
    if blocks.is_empty() {
        blocks.push(serde_json::json!({"type": "text", "text": ""}));
    }
    Ok(blocks)
}

/// OpenAI Chat API `content` — string for plain text, array of parts when images present.
pub fn openai_user_content_value(
    entry: &ContextEntry,
    supports_images: bool,
    _resolver: &Arc<dyn ImageResolver>,
) -> serde_json::Value {
    let mut parts: Vec<serde_json::Value> = Vec::new();
    let mut any_image = false;
    for p in &entry.parts {
        match p {
            ContentPart::Text { text } if !text.is_empty() => {
                parts.push(serde_json::json!({"type":"text","text": text}));
            }
            ContentPart::Image { mime, source } => {
                any_image = true;
                if !supports_images {
                    parts.push(serde_json::json!({"type":"text","text": image_fallback_stub("attachment", mime)}));
                    continue;
                }
                match extract_base64(source, mime) {
                    Ok((m, b64)) => {
                        let url = format!("data:{m};base64,{b64}");
                        parts
                            .push(serde_json::json!({"type":"image_url","image_url":{"url": url}}));
                    }
                    Err(e) => parts.push(serde_json::json!({
                        "type":"text",
                        "text": image_stub_note("attachment", mime, &e.to_string()),
                    })),
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return serde_json::Value::String(String::new());
    }
    if !any_image && parts.len() == 1 {
        if let Some(t) = parts[0].get("text").and_then(|v| v.as_str()) {
            return serde_json::Value::String(t.to_string());
        }
    }
    serde_json::Value::Array(parts)
}

pub fn gemini_user_parts(
    entry: &ContextEntry,
    supports_images: bool,
    _resolver: &Arc<dyn ImageResolver>,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for p in &entry.parts {
        match p {
            ContentPart::Text { text } if !text.is_empty() => {
                out.push(serde_json::json!({"text": text}));
            }
            ContentPart::Image { mime, source } => {
                if !supports_images {
                    out.push(serde_json::json!({"text": image_fallback_stub("attachment", mime)}));
                    continue;
                }
                match extract_base64(source, mime) {
                    Ok((m, b64)) => {
                        out.push(serde_json::json!({"inline_data":{"mime_type": m,"data": b64}}));
                    }
                    Err(e) => out.push(serde_json::json!({
                        "text": image_stub_note("attachment", mime, &e.to_string()),
                    })),
                }
            }
            _ => {}
        }
    }
    if out.is_empty() {
        out.push(serde_json::json!({"text": ""}));
    }
    out
}

/// Legacy sync resolver used only by the Ollama adapter, which constructs its
/// images list inline. Kept synchronous for now — Ollama callers must invoke
/// `prepare_for_inference` first so this only ever sees Base64 source.
pub fn resolve_image_to_base64(
    mime_in: &str,
    source: &ImageSource,
    _resolver: &Arc<dyn ImageResolver>,
) -> Result<(String, String), AgentOSError> {
    extract_base64(source, mime_in)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{ContextEntry, ContextRole};

    fn tiny_png_b64() -> String {
        // 1x1 black PNG.
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNgAAIAAAUAAen63NgAAAAASUVORK5CYII=".to_string()
    }

    fn user_entry_with_image(mime: &str, src: ImageSource) -> ContextEntry {
        ContextEntry {
            role: ContextRole::User,
            parts: vec![
                ContentPart::Text {
                    text: "look".into(),
                },
                ContentPart::Image {
                    mime: mime.into(),
                    source: src,
                },
            ],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: agentos_types::ContextPartition::Active,
            category: agentos_types::ContextCategory::History,
            is_summary: false,
        }
    }

    #[tokio::test]
    async fn prepare_validates_base64_with_magic_bytes() {
        let mut ctx = ContextWindow::new(4);
        ctx.push(user_entry_with_image(
            "image/png",
            ImageSource::Base64 {
                data: tiny_png_b64(),
            },
        ));
        let resolver: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
        let client = reqwest::Client::new();
        let prepared = prepare_for_inference(&ctx, true, resolver, &client).await;
        let img = prepared
            .entries
            .iter()
            .flat_map(|e| e.parts.iter())
            .find(|p| matches!(p, ContentPart::Image { .. }))
            .expect("image part still present");
        assert!(matches!(
            img,
            ContentPart::Image {
                source: ImageSource::Base64 { .. },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn prepare_rejects_mime_spoof() {
        let mut ctx = ContextWindow::new(4);
        // Real PNG bytes, declared as JPEG → magic bytes should override and we
        // accept as PNG (not reject — but mime gets corrected).
        ctx.push(user_entry_with_image(
            "image/jpeg",
            ImageSource::Base64 {
                data: tiny_png_b64(),
            },
        ));
        let resolver: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
        let client = reqwest::Client::new();
        let prepared = prepare_for_inference(&ctx, true, resolver, &client).await;
        let img = prepared.entries[0].parts.iter().find_map(|p| match p {
            ContentPart::Image { mime, .. } => Some(mime.as_str()),
            _ => None,
        });
        assert_eq!(img, Some("image/png"));
    }

    #[tokio::test]
    async fn prepare_rejects_garbage_base64() {
        let mut ctx = ContextWindow::new(4);
        ctx.push(user_entry_with_image(
            "image/png",
            ImageSource::Base64 {
                data: "abcd".into(), // base64-decodes to 3 garbage bytes; no magic match
            },
        ));
        let resolver: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
        let client = reqwest::Client::new();
        let prepared = prepare_for_inference(&ctx, true, resolver, &client).await;
        // Image part dropped → text stub.
        assert!(!prepared.entries[0]
            .parts
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. })));
    }

    #[tokio::test]
    async fn prepare_capability_gate_emits_stub() {
        let mut ctx = ContextWindow::new(4);
        ctx.push(user_entry_with_image(
            "image/png",
            ImageSource::Base64 {
                data: tiny_png_b64(),
            },
        ));
        let resolver: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
        let client = reqwest::Client::new();
        let prepared = prepare_for_inference(&ctx, false, resolver, &client).await;
        // No Image part remaining when capability is off.
        assert!(!prepared.entries[0]
            .parts
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. })));
        // Stub text contains "model does not support vision"
        assert!(prepared.entries[0]
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .any(|t| t.contains("does not support vision")));
    }

    #[tokio::test]
    async fn prepare_per_turn_cap_drops_excess() {
        let mut ctx = ContextWindow::new(4);
        let mut parts = vec![ContentPart::Text {
            text: "many".into(),
        }];
        for _ in 0..7 {
            parts.push(ContentPart::Image {
                mime: "image/png".into(),
                source: ImageSource::Base64 {
                    data: tiny_png_b64(),
                },
            });
        }
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts,
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: agentos_types::ContextPartition::Active,
            category: agentos_types::ContextCategory::History,
            is_summary: false,
        });
        let resolver: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
        let client = reqwest::Client::new();
        let prepared = prepare_for_inference(&ctx, true, resolver, &client).await;
        let images = prepared.entries[0]
            .parts
            .iter()
            .filter(|p| matches!(p, ContentPart::Image { .. }))
            .count();
        assert_eq!(images, MAX_IMAGES_PER_TURN);
    }

    #[tokio::test]
    async fn fetch_url_blocks_loopback() {
        let client = reqwest::Client::new();
        let res = fetch_url_async(&client, "http://127.0.0.1/foo.png", "image/png").await;
        assert!(res.is_err(), "loopback must be blocked");
    }

    #[tokio::test]
    async fn fetch_url_blocks_link_local() {
        let client = reqwest::Client::new();
        let res = fetch_url_async(&client, "http://169.254.169.254/latest/meta", "image/png").await;
        assert!(res.is_err(), "link-local IMDS must be blocked");
    }

    #[tokio::test]
    async fn fetch_url_blocks_private_range() {
        let client = reqwest::Client::new();
        let res = fetch_url_async(&client, "http://10.0.0.5/img.png", "image/png").await;
        assert!(res.is_err(), "private 10/8 must be blocked");
    }

    #[test]
    fn parse_data_uri_requires_base64() {
        assert!(parse_data_uri("data:image/png,raw").is_err());
    }

    #[test]
    fn data_uri_round_trip() {
        let url = format!("data:image/png;base64,{}", tiny_png_b64());
        let (m, _b64) = parse_data_uri(&url).expect("valid data uri");
        assert_eq!(m, "image/png");
    }

    #[test]
    fn supported_mime_allowlist() {
        assert!(is_supported_image_mime("image/png"));
        assert!(is_supported_image_mime("IMAGE/JPEG"));
        assert!(!is_supported_image_mime("image/svg+xml"));
        assert!(!is_supported_image_mime("application/pdf"));
    }
}
