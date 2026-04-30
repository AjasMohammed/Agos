use crate::auth::file_owner_principal;
use crate::auth::AuthToken;
use crate::file_store::{sanitize_display_name, sanitize_storage_name, UploadedFile};
use crate::state::AppState;
use agentos_llm::media::{is_supported_image_mime, MAX_INLINE_IMAGE_BYTES};
use agentos_types::{AgentOSError, ContentPart, ImageSource};
use axum::extract::{Extension, Multipart, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use base64::Engine;
use chrono::Utc;
use minijinja::context;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

// CSRF is validated by the global middleware via X-CSRF-Token header before these handlers run.

/// 100 MiB upload cap — enforced by streaming chunk accumulation.
const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;
const MAX_FILENAME_LEN: usize = 255;
/// Vision uploads per user message (matches adapter budgeting).
const MAX_IMAGE_PARTS_PER_TURN: usize = 5;

// ── Page handlers ──────────────────────────────────────────────────────────

/// GET /files — file management page with upload form and file list.
pub async fn list(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
) -> Response {
    let principal = file_owner_principal(&jar, &headers, &auth);
    let files = load_files(&state, &principal).await;

    let files_ctx: Vec<_> = files
        .iter()
        .map(|f| {
            context! {
                id          => f.id.clone(),
                name        => f.name.clone(),
                original_name => f.original_name.clone(),
                mime        => f.mime.clone(),
                size_kb     => f.size.saturating_add(1023) / 1024,
                tags        => f.tags.join(", "),
                uploaded_at => f.uploaded_at.clone(),
                is_text     => is_text_mime(&f.mime),
            }
        })
        .collect();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title  => "Files",
        breadcrumbs => vec![context! { label => "Files" }],
        files       => files_ctx,
        csrf_token,
    };
    super::render(&state.templates, "files.html", ctx)
}

/// POST /files/upload — multipart upload from the /files page (redirects on success).
pub async fn upload(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    multipart: Multipart,
) -> Response {
    let principal = file_owner_principal(&jar, &headers, &auth);
    match process_upload(multipart, &state, &principal, "global").await {
        Ok(_) => Redirect::to("/files").into_response(),
        Err(resp) => resp,
    }
}

/// POST /api/files/upload — AJAX upload from chat interface, returns JSON {id, name, original_name}.
pub async fn upload_api(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    multipart: Multipart,
) -> Response {
    let principal = file_owner_principal(&jar, &headers, &auth);
    // The scope is extracted from a `scope` field inside the multipart body by process_upload.
    // Default to "global" if the frontend doesn't send one.
    match process_upload(multipart, &state, &principal, "global").await {
        Ok(f) => axum::Json(serde_json::json!({
            "id":            f.id,
            "name":          f.name,
            "original_name": f.original_name,
            "scope":         f.scope,
        }))
        .into_response(),
        Err(resp) => resp,
    }
}

/// POST /files/{id}/delete — remove a file record and its bytes from disk.
pub async fn delete(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    Path(file_id): Path<String>,
) -> Response {
    if Uuid::parse_str(&file_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid file ID").into_response();
    }

    let principal = file_owner_principal(&jar, &headers, &auth);
    let store = Arc::clone(&state.file_store);
    let fid = file_id.clone();
    let p = principal.clone();
    match tokio::task::spawn_blocking(move || store.delete_file(&fid, &p)).await {
        Ok(Ok(Some(path))) => {
            // Canonicalize and verify the stored path is inside uploads_dir before removal.
            let uploads_dir = state.file_store.uploads_dir.clone();
            let disk_path = std::path::Path::new(&path);
            match (disk_path.canonicalize(), uploads_dir.canonicalize()) {
                (Ok(canon), Ok(up)) if canon.starts_with(&up) => {
                    if let Err(e) = tokio::fs::remove_file(&canon).await {
                        tracing::warn!(
                            file_id = %file_id, path = %path, error = %e,
                            "DB row deleted but on-disk removal failed — bytes may be orphaned"
                        );
                    } else {
                        tracing::info!(file_id = %file_id, "File deleted");
                    }
                }
                (Ok(canon), Ok(_)) => {
                    // Path resolved but is outside uploads_dir — don't unlink, but warn.
                    tracing::warn!(
                        file_id = %file_id, resolved = %canon.display(),
                        "DB row deleted but disk path is outside uploads_dir — not unlinking"
                    );
                }
                (Err(e), _) => {
                    // File already gone from disk or unreadable — DB row is removed, that's fine.
                    tracing::debug!(
                        file_id = %file_id, path = %path, error = %e,
                        "File not found on disk during delete (already removed?)"
                    );
                }
                _ => {
                    tracing::warn!(file_id = %file_id, "Could not canonicalize uploads_dir during delete");
                }
            }
        }
        Ok(Ok(None)) => {} // already gone
        _ => return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete file").into_response(),
    }

    Redirect::to("/files").into_response()
}

/// GET /files/{id}/download — serve the raw file bytes with correct headers.
pub async fn download(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    Path(file_id): Path<String>,
) -> Response {
    if Uuid::parse_str(&file_id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid file ID").into_response();
    }

    let principal = file_owner_principal(&jar, &headers, &auth);
    let store = Arc::clone(&state.file_store);
    let fid = file_id.clone();
    let p = principal.clone();
    let record = match tokio::task::spawn_blocking(move || store.get_file(&fid, &p)).await {
        Ok(Ok(Some(r))) => r,
        Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "File not found").into_response(),
        _ => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response(),
    };

    // SECURITY: verify the stored path is still inside the uploads directory.
    let uploads_dir = state.file_store.uploads_dir.clone();
    let disk_path = std::path::PathBuf::from(&record.path);
    let canonical = match disk_path.canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "File not found on disk").into_response(),
    };
    let canonical_uploads = match uploads_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response(),
    };
    if !canonical.starts_with(&canonical_uploads) {
        return (StatusCode::FORBIDDEN, "Access denied").into_response();
    }

    let bytes = match tokio::fs::read(&canonical).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::NOT_FOUND, "File not found on disk").into_response(),
    };

    // Strip quotes from filename to prevent header injection.
    let safe_filename = record.original_name.replace(['"', '\r', '\n'], "");
    let disposition = format!("attachment; filename=\"{safe_filename}\"");

    // Never trust the client-supplied MIME for the download Content-Type — an attacker
    // could upload a file with Content-Type: text/html and get stored-XSS via inline
    // rendering. Force octet-stream for everything outside a small safe allowlist.
    let safe_mime = safe_download_mime(&record.mime);

    (
        [
            (header::CONTENT_TYPE, safe_mime),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response()
}

/// Return a safe Content-Type for file downloads.
/// Only MIME types that cannot execute scripts or trigger renderer-side side effects
/// are passed through; everything else becomes `application/octet-stream`.
fn safe_download_mime(mime: &str) -> String {
    let lower = mime.to_lowercase();
    let allowed = lower == "application/octet-stream"
        || lower == "application/pdf"
        || lower == "application/zip"
        || lower == "application/gzip"
        || lower.starts_with("image/")
        || lower.starts_with("audio/")
        || lower.starts_with("video/")
        || lower.starts_with("text/plain")
        || lower.starts_with("text/csv")
        || lower.starts_with("text/markdown")
        || lower.starts_with("text/x-")
        || lower.starts_with("application/json")
        || lower.starts_with("application/x-ndjson");
    if allowed {
        mime.to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

// ── Shared upload logic ────────────────────────────────────────────────────

/// Parse a multipart upload, write to disk, register in FileStore.
/// Returns the registered `UploadedFile` on success, or an error response.
///
/// CSRF is validated by the global middleware via `X-CSRF-Token` header before this runs.
/// The DefaultBodyLimit on these routes caps the request at 100 MiB + 1 MiB form slack;
/// the per-field chunk accumulation below is a secondary defence-in-depth cap.
async fn process_upload(
    mut multipart: Multipart,
    state: &AppState,
    owner_principal: &str,
    default_scope: &str,
) -> Result<UploadedFile, Response> {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name = String::from("upload");
    let mut file_mime = String::from("application/octet-stream");
    let mut tags = String::new();
    let mut upload_scope = default_scope.to_string();

    while let Ok(Some(mut field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "tags" => {
                if let Ok(b) = field.bytes().await {
                    tags = String::from_utf8_lossy(&b).trim().to_string();
                    // Only alphanumeric, spaces, commas, hyphens allowed in tags.
                    tags.retain(|c: char| c.is_alphanumeric() || " ,-_".contains(c));
                    // Server-side cap — the client maxlength is advisory only.
                    tags.truncate(200);
                }
            }
            "file" => {
                let raw_name = field
                    .file_name()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "upload".to_string());

                // Reject path traversal components in filenames.
                if raw_name.contains("..") || raw_name.contains('/') || raw_name.contains('\\') {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "Filename contains invalid characters",
                    )
                        .into_response());
                }
                if raw_name.len() > MAX_FILENAME_LEN {
                    return Err(
                        (StatusCode::BAD_REQUEST, "Filename too long (max 255 chars)")
                            .into_response(),
                    );
                }

                file_name = raw_name;
                file_mime = field
                    .content_type()
                    .map(|ct| ct.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                let mut buf = Vec::new();
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            buf.extend_from_slice(&chunk);
                            if buf.len() > MAX_UPLOAD_BYTES {
                                return Err((
                                    StatusCode::PAYLOAD_TOO_LARGE,
                                    "File too large (max 100 MiB)",
                                )
                                    .into_response());
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(error = %e, "Error reading upload chunk");
                            return Err((StatusCode::BAD_REQUEST, "Failed to read uploaded data")
                                .into_response());
                        }
                    }
                }

                // Accept zero-byte files — an empty file is a valid upload.
                file_bytes = Some(buf);
            }
            _ => {
                // Check for known extra fields like `scope`.
                if field_name == "scope" {
                    if let Ok(b) = field.bytes().await {
                        let s = String::from_utf8_lossy(&b).trim().to_string();
                        // Only allow well-formed scope values.
                        if s == "global" {
                            upload_scope = s;
                        } else if let Some(session_part) = s.strip_prefix("session:") {
                            // Validate the session portion is a proper UUID.
                            if uuid::Uuid::parse_str(session_part).is_ok() {
                                upload_scope = s;
                            }
                        }
                    }
                }
                // Skip other unknown fields.
            }
        }
    }

    let bytes = match file_bytes {
        Some(b) => b,
        None => return Err((StatusCode::BAD_REQUEST, "No file provided").into_response()),
    };

    let fmime_lc = file_mime.to_ascii_lowercase();
    if fmime_lc.starts_with("image/")
        && is_supported_image_mime(&fmime_lc)
        && bytes.len() > MAX_INLINE_IMAGE_BYTES
    {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Image uploads are limited to 5 MiB",
        )
            .into_response());
    }

    // Build a safe on-disk filename: {uuid}_{sanitized_original}.
    let file_id = Uuid::new_v4().to_string();
    let safe_part = sanitize_storage_name(&file_name);
    let stored_name = format!("{file_id}_{safe_part}");
    let disk_path = state.file_store.uploads_dir.join(&stored_name);
    let disk_path_str = disk_path.to_string_lossy().to_string();
    let size = bytes.len() as u64;

    let store = Arc::clone(&state.file_store);
    let fid = file_id.clone();
    let fname = file_name.clone();
    let fmime = file_mime.clone();
    let ftags = tags.clone();
    let owner = owner_principal.to_string();
    let fscope = upload_scope.clone();
    // Clone for use after the closure (closure moves disk_path and disk_path_str).
    let disk_path_str_ret = disk_path_str.clone();

    match tokio::task::spawn_blocking(move || -> Result<(), String> {
        std::fs::write(&disk_path, &bytes).map_err(|e| format!("write to disk: {e}"))?;
        if let Err(e) = store.register_file(
            &fid,
            &fname,
            &fmime,
            size,
            &disk_path_str,
            &ftags,
            &owner,
            &fscope,
        ) {
            // Best-effort cleanup: remove the orphaned file if DB registration fails.
            let _ = std::fs::remove_file(&disk_path_str);
            return Err(format!("register in db: {e}"));
        }
        Ok(())
    })
    .await
    {
        Ok(Ok(())) => {
            tracing::info!(
                file_id = %file_id,
                filename = %file_name,
                size,
                "File uploaded"
            );
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "Failed to store uploaded file");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to store file").into_response());
        }
        Err(e) => {
            tracing::error!(error = %e, "spawn_blocking panicked during upload");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response());
        }
    }

    Ok(UploadedFile {
        id: file_id,
        name: sanitize_display_name(&file_name),
        original_name: file_name,
        mime: file_mime,
        size,
        path: disk_path_str_ret,
        tags: tags
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        uploaded_at: Utc::now().to_rfc3339(),
        scope: upload_scope,
    })
}

// ── Helpers ────────────────────────────────────────────────────────────────

async fn load_files(state: &AppState, owner_principal: &str) -> Vec<UploadedFile> {
    let store = Arc::clone(&state.file_store);
    let p = owner_principal.to_string();
    tokio::task::spawn_blocking(move || store.list_files(&p, Some("global")))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "load_files: spawn_blocking panicked");
            Ok(vec![])
        })
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "load_files: db query failed");
            vec![]
        })
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || mime.contains("javascript")
        || mime.contains("yaml")
        || mime.contains("toml")
        || mime.contains("markdown")
}

/// Escape characters that would be unsafe inside an HTML attribute value.
fn escape_html_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Escape any `</user_data>` (case-insensitive) inside file content so an uploaded
/// file cannot close the wrapping `<user_data>` tag early and inject prompt text.
fn escape_user_data_close(s: &str) -> std::borrow::Cow<'_, str> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| regex::Regex::new(r"(?i)</user_data>").expect("static regex is valid"));
    re.replace_all(s, "&lt;/user_data&gt;")
}

/// Resolve file IDs into typed context parts ([`FileStore`] lookup + path safety).
/// Kept separate from [`AppState`] so unit tests can exercise the image branch without booting the kernel.
///
/// Text MIMEs become [`ContentPart::Text`]; allowlisted images become [`ContentPart::Image`] when `supports_images`.
pub async fn resolve_file_ids_with_store(
    ids_csv: &str,
    file_store: Arc<crate::file_store::FileStore>,
    owner_principal: &str,
    supports_images: bool,
) -> Vec<ContentPart> {
    if ids_csv.trim().is_empty() {
        return Vec::new();
    }

    let ids: Vec<String> = ids_csv
        .split(',')
        .map(str::trim)
        .filter(|s| Uuid::parse_str(s).is_ok())
        .take(20)
        .map(String::from)
        .collect();

    if ids.is_empty() {
        return Vec::new();
    }

    let store = Arc::clone(&file_store);
    let owner = owner_principal.to_string();

    let records =
        match tokio::task::spawn_blocking(move || store.get_files_by_ids(&ids, &owner)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "resolve_file_ids_to_context: DB lookup failed");
                return Vec::new();
            }
            Err(e) => {
                tracing::warn!(error = %e, "resolve_file_ids_to_context: spawn_blocking panicked");
                return Vec::new();
            }
        };

    let canonical_uploads = match file_store.uploads_dir.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "resolve_file_ids_to_context: canonicalize uploads_dir failed");
            return Vec::new();
        }
    };

    let mut out: Vec<ContentPart> = Vec::with_capacity(records.len());
    let mut image_parts_used = 0usize;

    for record in &records {
        let disk_path = std::path::PathBuf::from(&record.path);

        let canonical = match disk_path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    file_id = %record.id,
                    error = %e,
                    "resolve_file_ids_to_context: file not found on disk"
                );
                out.push(ContentPart::Text {
                    text: format!(
                        "[Attached: {} — file not found]\n",
                        escape_html_attr(&record.original_name)
                    ),
                });
                continue;
            }
        };
        if !canonical.starts_with(&canonical_uploads) {
            tracing::warn!(file_id = %record.id, "resolve_file_ids_to_context: path escapes uploads_dir");
            continue;
        }

        let safe_name = escape_html_attr(&record.original_name);
        let file_id = escape_html_attr(&record.id);
        let mime_lc = record.mime.to_ascii_lowercase();

        if is_text_mime(&record.mime) {
            match tokio::fs::read_to_string(&canonical).await {
                Ok(content) => {
                    const MAX_INLINE: usize = 1024 * 1024;
                    if content.len() > MAX_INLINE {
                        let cut = content
                            .char_indices()
                            .take_while(|(i, _)| *i < MAX_INLINE)
                            .last()
                            .map(|(i, c)| i + c.len_utf8())
                            .unwrap_or(0);
                        let safe_body = escape_user_data_close(&content[..cut]);
                        out.push(ContentPart::Text {
                            text: format!(
                                "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\" truncated=\"true\" total_bytes=\"{}\">\n\
                                 {safe_body}\n\
                                 [... truncated at 1 MiB — {} total bytes. \
                                 To read the full file, use the user-file-reader tool with file_id=\"{}\"]\n\
                                 </user_data>\n",
                                content.len(),
                                content.len(),
                                record.id
                            ),
                        });
                    } else {
                        let safe_body = escape_user_data_close(&content);
                        out.push(ContentPart::Text {
                            text: format!(
                                "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\">\n{safe_body}\n</user_data>\n"
                            ),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(file_id = %record.id, error = %e, "resolve_file_ids_to_context: could not read file");
                    out.push(ContentPart::Text {
                        text: format!("[Attached: {safe_name} — could not read file]\n"),
                    });
                }
            }
        } else if mime_lc.starts_with("image/")
            && supports_images
            && is_supported_image_mime(&mime_lc)
        {
            if image_parts_used >= MAX_IMAGE_PARTS_PER_TURN {
                out.push(ContentPart::Text {
                    text: format!(
                        "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\" note=\"image limit per turn reached ({MAX_IMAGE_PARTS_PER_TURN}); use user-file-reader\" />\n"
                    ),
                });
                continue;
            }
            match tokio::fs::read(&canonical).await {
                Ok(bytes) if bytes.len() <= MAX_INLINE_IMAGE_BYTES => {
                    let safe_mime = escape_html_attr(&record.mime);
                    out.push(ContentPart::Text {
                        text: format!(
                            "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\" type=\"image\" mime=\"{safe_mime}\" />\n"
                        ),
                    });
                    out.push(ContentPart::Image {
                        mime: record.mime.clone(),
                        source: ImageSource::FileRef {
                            file_id: record.id.clone(),
                        },
                    });
                    image_parts_used += 1;
                }
                Ok(bytes) => {
                    let safe_mime = escape_html_attr(&record.mime);
                    out.push(ContentPart::Text {
                        text: format!(
                            "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\" type=\"binary\" size_kib=\"{}\" mime=\"{safe_mime}\" \
                             note=\"Image too large for inline vision ({} bytes). Use user-file-reader tool with file_id=&quot;{}&quot;.\" />\n",
                            bytes.len().saturating_add(1023) / 1024,
                            bytes.len(),
                            record.id,
                        ),
                    });
                }
                Err(e) => {
                    tracing::warn!(file_id = %record.id, error = %e, "resolve_file_ids_to_context: could not read image");
                    let safe_mime = escape_html_attr(&record.mime);
                    out.push(ContentPart::Text {
                        text: format!(
                            "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\" type=\"binary\" mime=\"{safe_mime}\" note=\"could not read file\" />\n"
                        ),
                    });
                }
            }
        } else {
            let safe_mime = escape_html_attr(&record.mime);
            out.push(ContentPart::Text {
                text: format!(
                    "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\" type=\"binary\" size_kib=\"{}\" mime=\"{safe_mime}\" \
                     note=\"Binary file attached. Use user-file-reader tool with file_id=&quot;{}&quot; to read contents.\" />\n",
                    record.size.saturating_add(1023) / 1024,
                    record.id,
                ),
            });
        }
    }

    out
}

/// Same as [`resolve_file_ids_with_store`], using the web [`AppState`] file store.
pub async fn resolve_file_ids_to_context(
    ids_csv: &str,
    state: &AppState,
    owner_principal: &str,
    supports_images: bool,
) -> Vec<ContentPart> {
    resolve_file_ids_with_store(
        ids_csv,
        Arc::clone(&state.file_store),
        owner_principal,
        supports_images,
    )
    .await
}

/// Resolve `@filename` mentions in a message string to inline file content.
/// Looks up each mention in the FileStore and prepends the content.
/// `session_id` is used to also search session-scoped files.
pub async fn resolve_at_mentions(
    message: &str,
    state: &AppState,
    owner_principal: &str,
    session_id: Option<&str>,
) -> String {
    // Simple pattern: @word or @word.ext — alphanumeric, dot, dash, underscore.
    let re = match regex::Regex::new(r"@([\w.\-]+)") {
        Ok(r) => r,
        Err(_) => return message.to_string(),
    };

    let mentions: Vec<String> = re
        .captures_iter(message)
        .map(|cap| cap[1].to_string())
        .collect();

    if mentions.is_empty() {
        return message.to_string();
    }

    let mut preamble = String::new();
    let store = Arc::clone(&state.file_store);
    let owner = owner_principal.to_string();

    // Canonicalize uploads_dir once — not inside the loop (blocking syscall).
    let canonical_uploads = match state.file_store.uploads_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => return message.to_string(),
    };

    for mention in &mentions {
        let m = mention.clone();
        let s = Arc::clone(&store);
        let o = owner.clone();
        let sid = session_id.map(|s| s.to_string());
        let record =
            match tokio::task::spawn_blocking(move || s.find_by_name(&m, &o, sid.as_deref())).await
            {
                Ok(Ok(Some(r))) => r,
                _ => continue,
            };

        let disk_path = std::path::PathBuf::from(&record.path);
        let canonical = match disk_path.canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if !canonical.starts_with(&canonical_uploads) {
            continue;
        }

        let safe_name = escape_html_attr(&record.original_name);
        let file_id = escape_html_attr(&record.id);
        if is_text_mime(&record.mime) {
            if let Ok(content) = tokio::fs::read_to_string(&canonical).await {
                const MAX_INLINE: usize = 512 * 1024;
                if content.len() > MAX_INLINE {
                    let cut = content
                        .char_indices()
                        .take_while(|(i, _)| *i < MAX_INLINE)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(0);
                    let safe_body = escape_user_data_close(&content[..cut]);
                    preamble.push_str(&format!(
                        "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\" truncated=\"true\" total_bytes=\"{}\">\n\
                         {safe_body}\n\
                         [... truncated at 512 KiB — {} total bytes. \
                         To read the full file, use the user-file-reader tool with file_id=\"{}\"]\n\
                         </user_data>\n\n",
                        content.len(), content.len(), record.id
                    ));
                } else {
                    let safe_body = escape_user_data_close(&content);
                    preamble.push_str(&format!(
                        "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\">\n{safe_body}\n</user_data>\n\n"
                    ));
                }
            }
        } else {
            let safe_mime = escape_html_attr(&record.mime);
            preamble.push_str(&format!(
                "<user_data filename=\"{safe_name}\" file_id=\"{file_id}\" type=\"binary\" size_kib=\"{}\" mime=\"{safe_mime}\" \
                 note=\"Binary file attached. Use user-file-reader tool with file_id=&quot;{}&quot; to read contents.\" />\n\n",
                record.size.saturating_add(1023) / 1024, record.id,
            ));
        }
    }

    if preamble.is_empty() {
        message.to_string()
    } else {
        format!("{preamble}---\n{message}")
    }
}

/// Resolves uploaded file IDs to `(mime, base64)` for multimodal LLM adapters.
pub struct FileStoreImageResolver {
    store: Arc<crate::file_store::FileStore>,
    uploads_canon: std::path::PathBuf,
}

impl FileStoreImageResolver {
    pub fn new(store: Arc<crate::file_store::FileStore>) -> Result<Self, std::io::Error> {
        Ok(Self {
            uploads_canon: store.uploads_dir.canonicalize()?,
            store,
        })
    }
}

impl agentos_llm::ImageResolver for FileStoreImageResolver {
    fn resolve_filename(&self, file_id: &str) -> Option<String> {
        self.store
            .get_file_by_id_unscoped(file_id)
            .ok()
            .flatten()
            .map(|r| r.original_name)
    }

    fn resolve_base64(&self, file_id: &str) -> Result<(String, String), AgentOSError> {
        let record =
            self.store
                .get_file_by_id_unscoped(file_id)
                .map_err(|e| AgentOSError::KernelError {
                    reason: format!("file lookup: {e}"),
                })?;
        let Some(record) = record else {
            return Err(AgentOSError::LLMError {
                provider: "file-store".to_string(),
                reason: format!("unknown file_id {file_id}"),
            });
        };
        let disk_path = std::path::PathBuf::from(&record.path);
        let canonical = disk_path
            .canonicalize()
            .map_err(|e| AgentOSError::KernelError {
                reason: format!("canonicalize: {e}"),
            })?;
        if !canonical.starts_with(&self.uploads_canon) {
            return Err(AgentOSError::KernelError {
                reason: "path escapes uploads directory".into(),
            });
        }
        let bytes = std::fs::read(&canonical).map_err(|e| AgentOSError::KernelError {
            reason: format!("read: {e}"),
        })?;
        if bytes.len() > MAX_INLINE_IMAGE_BYTES {
            return Err(AgentOSError::SchemaValidation(format!(
                "image exceeds max inline size ({} bytes)",
                MAX_INLINE_IMAGE_BYTES
            )));
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok((record.mime, b64))
    }
}

/// GET /api/files/search?q=...&session_id=... — fuzzy file search for the @mention typeahead.
pub async fn search_api(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Extension(auth): Extension<AuthToken>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let principal = file_owner_principal(&jar, &headers, &auth);
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let session_id = params.get("session_id").map(|s| s.as_str());

    // Validate session_id if provided.
    if let Some(sid) = session_id {
        if uuid::Uuid::parse_str(sid).is_err() {
            return (StatusCode::BAD_REQUEST, "Invalid session_id").into_response();
        }
    }

    let store = Arc::clone(&state.file_store);
    let p = principal.clone();
    let q = query.to_string();
    let sid = session_id.map(String::from);

    let results =
        match tokio::task::spawn_blocking(move || store.search_files(&q, &p, sid.as_deref(), 20))
            .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "File search failed");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Search failed").into_response();
            }
            Err(e) => {
                tracing::error!(error = %e, "File search task panicked");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
            }
        };

    let items: Vec<serde_json::Value> = results
        .iter()
        .map(|f| {
            serde_json::json!({
                "id": f.id,
                "name": f.name,
                "original_name": f.original_name,
                "mime": f.mime,
                "size_kb": f.size.saturating_add(1023) / 1024,
                "scope": f.scope,
            })
        })
        .collect();

    axum::Json(serde_json::json!({ "files": items })).into_response()
}

#[cfg(test)]
mod resolve_multimodal_tests {
    use super::resolve_file_ids_with_store;
    use crate::file_store::FileStore;
    use agentos_types::{ContentPart, ImageSource};
    use base64::Engine;
    use std::sync::Arc;

    fn tiny_png_bytes() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
            .expect("fixture png")
    }

    async fn seeded_png_upload(
        owner: &str,
        id_str: &'static str,
        mime: &str,
    ) -> (Arc<FileStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileStore::open(dir.path()).expect("store"));
        let path = store.uploads_dir.join(format!("{id_str}_f.png"));
        let bytes = if mime == "image/heic" {
            vec![0u8; 32]
        } else {
            tiny_png_bytes()
        };
        std::fs::write(&path, &bytes).expect("write file");
        let path_str = path.to_string_lossy().to_string();
        store
            .register_file(
                id_str,
                "f.png",
                mime,
                bytes.len() as u64,
                &path_str,
                "",
                owner,
                "global",
            )
            .expect("register");
        (store, dir)
    }

    #[tokio::test]
    async fn image_mime_produces_image_part() {
        let owner = "owner_hash_integration_test____________";
        let id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let (store, _dir) = seeded_png_upload(owner, id, "image/png").await;
        let parts = resolve_file_ids_with_store(id, Arc::clone(&store), owner, true).await;
        assert!(parts.iter().any(|p| matches!(p, ContentPart::Image { .. })));
        let img = parts
            .iter()
            .find_map(|p| match p {
                ContentPart::Image { mime, source } => Some((mime, source)),
                _ => None,
            })
            .expect("image part");
        assert_eq!(img.0.as_str(), "image/png");
        assert!(matches!(
            img.1,
            ImageSource::FileRef {
                ref file_id
            } if file_id == id
        ));
    }

    #[tokio::test]
    async fn image_mime_falls_back_when_unsupported_adapter() {
        let owner = "owner_hash_integration_test____________";
        let id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let (store, _dir) = seeded_png_upload(owner, id, "image/png").await;
        let parts = resolve_file_ids_with_store(id, Arc::clone(&store), owner, false).await;
        assert!(!parts.iter().any(|p| matches!(p, ContentPart::Image { .. })));
        let t = parts
            .iter()
            .find_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .expect("stub text");
        assert!(t.contains("Binary") || t.contains("image"));
    }

    #[tokio::test]
    async fn per_turn_image_cap_stubs_extras() {
        let owner = "owner_per_turn_cap_test________________";
        let png = tiny_png_bytes();
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileStore::open(dir.path()).expect("store"));
        let uuids = [
            "f1111111-1111-1111-1111-111111111101",
            "f1111111-1111-1111-1111-111111111102",
            "f1111111-1111-1111-1111-111111111103",
            "f1111111-1111-1111-1111-111111111104",
            "f1111111-1111-1111-1111-111111111105",
            "f1111111-1111-1111-1111-111111111106",
            "f1111111-1111-1111-1111-111111111107",
        ];
        for (i, id) in uuids.iter().enumerate() {
            let path = store.uploads_dir.join(format!("{id}_t.png"));
            std::fs::write(&path, &png).expect("write");
            let path_str = path.to_string_lossy().to_string();
            store
                .register_file(
                    id,
                    &format!("t{i}.png"),
                    "image/png",
                    png.len() as u64,
                    &path_str,
                    "",
                    owner,
                    "global",
                )
                .expect("register");
        }
        let csv = uuids.join(",");
        let parts = resolve_file_ids_with_store(&csv, Arc::clone(&store), owner, true).await;
        let img_count = parts
            .iter()
            .filter(|p| matches!(p, ContentPart::Image { .. }))
            .count();
        assert_eq!(
            img_count, 5,
            "expected 5 native image parts, got {img_count}"
        );
        let stub_hints = parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .filter(|t| t.contains("image limit per turn"))
            .count();
        assert!(stub_hints >= 1, "expected stub for overflow images");
    }

    #[tokio::test]
    async fn non_allowlisted_image_mime_skips_native_image_part() {
        let owner = "owner_hash_integration_test____________";
        let id = "cccccccc-cccc-cccc-cccc-cccccccccccc";
        let (store, _dir) = seeded_png_upload(owner, id, "image/heic").await;
        let parts = resolve_file_ids_with_store(id, Arc::clone(&store), owner, true).await;
        assert!(!parts.iter().any(|p| matches!(p, ContentPart::Image { .. })));
    }
}
