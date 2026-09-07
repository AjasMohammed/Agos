//! File-store endpoints: upload (multipart), list, get, download, delete.
//!
//! Upload/download logic is ported from `agentos-web`'s file handlers — the same
//! 100 MiB cap, path-traversal rejection, `{uuid}_{sanitized}` on-disk naming,
//! image-MIME 5 MiB cap, canonicalize-and-`starts_with` traversal guard, and the
//! `safe_download_mime` allowlist. The owner principal is the authenticated API
//! key id (`key.0.id`).

use axum::body::Bytes;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use std::sync::Arc;

use super::require_permission;
use crate::auth::AuthenticatedKey;
use crate::error::ApiError;
use crate::response::{Envelope, ListEnvelope};
use crate::service::KernelService;
use crate::types::{ApiFileMeta, FileListQuery};

/// 100 MiB upload cap, enforced by chunk accumulation in `process_upload`.
const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;
const MAX_FILENAME_LEN: usize = 255;

/// `POST /api/v1/files` — Upload a file via `multipart/form-data`.
///
/// Fields: `file` (required), `tags` (optional CSV-ish), `scope` (optional —
/// `"global"` or `"session:{uuid}"`, defaults to `"global"`).
#[utoipa::path(
    post,
    path = "/api/v1/files",
    tag = "files",
    operation_id = "files_upload",
    request_body(content = String, description = "Multipart form: file, tags?, scope?", content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Uploaded file metadata", body = crate::response::Envelope<crate::types::ApiFileMeta>),
        (status = 400, description = "Bad request", body = crate::error::ApiErrorBody),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 413, description = "Payload too large", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn upload(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    multipart: Multipart,
) -> Result<Json<Envelope<ApiFileMeta>>, ApiError> {
    require_permission(&key, "files:w")?;
    let owner = key.0.id.clone();
    let parsed = parse_upload(multipart).await?;
    let meta = svc
        .upload_file(
            &owner,
            &parsed.original_name,
            &parsed.mime,
            &parsed.scope,
            &parsed.tags,
            parsed.bytes,
        )
        .await?;
    Ok(Json(Envelope::new(meta)))
}

/// `GET /api/v1/files` — List files for the authenticated principal.
#[utoipa::path(
    get,
    path = "/api/v1/files",
    tag = "files",
    operation_id = "files_list",
    params(FileListQuery),
    responses(
        (status = 200, description = "List of files", body = crate::response::ListEnvelope<crate::types::ApiFileMeta>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Query(q): Query<FileListQuery>,
) -> Result<Json<ListEnvelope<ApiFileMeta>>, ApiError> {
    require_permission(&key, "files:r")?;
    let files = svc
        .list_files(
            &key.0.id,
            q.scope.as_deref(),
            q.tag.as_deref(),
            q.q.as_deref(),
        )
        .await?;
    let total = files.len() as u64;
    Ok(Json(ListEnvelope::new(files, total)))
}

/// `GET /api/v1/files/{id}` — Get a single file's metadata.
#[utoipa::path(
    get,
    path = "/api/v1/files/{id}",
    tag = "files",
    operation_id = "files_get",
    params(("id" = String, Path, description = "File id (UUID)")),
    responses(
        (status = 200, description = "File metadata", body = crate::response::Envelope<crate::types::ApiFileMeta>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "File not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<ApiFileMeta>>, ApiError> {
    require_permission(&key, "files:r")?;
    let meta = svc.get_file(&key.0.id, &id).await?;
    Ok(Json(Envelope::new(meta)))
}

/// `GET /api/v1/files/{id}/download` — Stream the raw file bytes.
///
/// This route returns raw bytes (NOT the `{ data }` envelope) with a safe
/// `Content-Type` (allowlisted via the kernel) and a header-safe
/// `Content-Disposition`.
#[utoipa::path(
    get,
    path = "/api/v1/files/{id}/download",
    tag = "files",
    operation_id = "files_download",
    params(("id" = String, Path, description = "File id (UUID)")),
    responses(
        (status = 200, description = "Raw file bytes", content_type = "application/octet-stream"),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "File not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn download(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    require_permission(&key, "files:r")?;
    let (mime, name, bytes) = svc.download_file(&key.0.id, &id).await?;
    // Strip quotes/CRLF from filename to prevent header injection.
    let safe_filename = name.replace(['"', '\r', '\n'], "");
    let disposition = format!("attachment; filename=\"{safe_filename}\"");
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response())
}

/// `DELETE /api/v1/files/{id}` — Remove a file record and its bytes from disk.
#[utoipa::path(
    delete,
    path = "/api/v1/files/{id}",
    tag = "files",
    operation_id = "files_delete",
    params(("id" = String, Path, description = "File id (UUID)")),
    responses(
        (status = 200, description = "File deleted", body = crate::response::Envelope<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = crate::error::ApiErrorBody),
        (status = 404, description = "File not found", body = crate::error::ApiErrorBody)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete(
    State(svc): State<Arc<dyn KernelService>>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<serde_json::Value>>, ApiError> {
    require_permission(&key, "files:w")?;
    svc.delete_file(&key.0.id, &id).await?;
    Ok(Json(Envelope::new(serde_json::json!({ "deleted": id }))))
}

// ── Multipart parsing ────────────────────────────────────────────────────────

/// Result of parsing the upload multipart body.
struct ParsedUpload {
    bytes: Vec<u8>,
    original_name: String,
    mime: String,
    tags: Vec<String>,
    scope: String,
}

/// Parse the multipart upload form, applying the same validation as the web
/// handler: path-traversal filename rejection, filename length cap, and the
/// 100 MiB chunk-accumulation cap. The image-MIME 5 MiB cap and on-disk write
/// happen in the kernel (`upload_file`).
async fn parse_upload(mut multipart: Multipart) -> Result<ParsedUpload, ApiError> {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name = String::from("upload");
    let mut file_mime = String::from("application/octet-stream");
    let mut tags = String::new();
    let mut scope = String::from("global");

    while let Ok(Some(mut field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "tags" => {
                if let Ok(b) = field.bytes().await {
                    let mut t = String::from_utf8_lossy(&b).trim().to_string();
                    // Only alphanumeric, spaces, commas, hyphens, underscores.
                    t.retain(|c: char| c.is_alphanumeric() || " ,-_".contains(c));
                    t.truncate(200);
                    tags = t;
                }
            }
            "scope" => {
                if let Ok(b) = field.bytes().await {
                    let s = String::from_utf8_lossy(&b).trim().to_string();
                    if s == "global" {
                        scope = s;
                    } else if let Some(session_part) = s.strip_prefix("session:") {
                        if uuid::Uuid::parse_str(session_part).is_ok() {
                            scope = s;
                        }
                    }
                }
            }
            "file" => {
                let raw_name = field
                    .file_name()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "upload".to_string());

                if raw_name.contains("..") || raw_name.contains('/') || raw_name.contains('\\') {
                    return Err(ApiError::BadRequest(
                        "Filename contains invalid characters".into(),
                    ));
                }
                if raw_name.len() > MAX_FILENAME_LEN {
                    return Err(ApiError::BadRequest(
                        "Filename too long (max 255 chars)".into(),
                    ));
                }

                file_name = raw_name;
                file_mime = field
                    .content_type()
                    .map(|ct| ct.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                let mut buf: Vec<u8> = Vec::new();
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            let chunk: Bytes = chunk;
                            buf.extend_from_slice(&chunk);
                            if buf.len() > MAX_UPLOAD_BYTES {
                                return Err(ApiError::BadRequest(
                                    "File too large (max 100 MiB)".into(),
                                ));
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(error = %e, "Error reading upload chunk");
                            return Err(ApiError::BadRequest(
                                "Failed to read uploaded data".into(),
                            ));
                        }
                    }
                }
                file_bytes = Some(buf);
            }
            _ => {}
        }
    }

    let bytes = file_bytes.ok_or_else(|| ApiError::BadRequest("No file provided".into()))?;
    let tags: Vec<String> = tags
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    Ok(ParsedUpload {
        bytes,
        original_name: file_name,
        mime: file_mime,
        tags,
        scope,
    })
}
