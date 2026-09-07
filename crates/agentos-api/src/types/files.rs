//! DTOs for the file-store REST surface (Phase 06).

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Metadata for a single uploaded file. Mirrors
/// `agentos_kernel::file_store::UploadedFile` minus the on-disk path (never
/// exposed to clients).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiFileMeta {
    /// Stable file id (UUID). Used in download/get/delete routes.
    pub id: String,
    /// Sanitized display name (used for @mention matching).
    pub name: String,
    /// Original filename as uploaded.
    pub original_name: String,
    /// Stored MIME type.
    pub mime: String,
    /// Size in bytes.
    pub size: u64,
    /// `"global"` for ecosystem files, `"session:{id}"` for chat-scoped files.
    pub scope: String,
    /// Free-form tags attached at upload time.
    pub tags: Vec<String>,
    /// RFC3339 (or `YYYY-MM-DDTHH:MM:SSZ`) upload timestamp.
    pub uploaded_at: String,
}

/// Query parameters for `GET /api/v1/files`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams)]
pub struct FileListQuery {
    /// Post-filter to files carrying this tag (FileStore has no native tag filter).
    #[serde(default)]
    pub tag: Option<String>,
    /// Fuzzy name search delegated to `FileStore::search_files`.
    #[serde(default)]
    pub q: Option<String>,
    /// Restrict to a scope (`"global"` or `"session:{id}"`). Omit for all scopes.
    #[serde(default)]
    pub scope: Option<String>,
}
