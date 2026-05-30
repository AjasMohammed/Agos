//! Typed success-response envelope for the REST API.
//!
//! Every successful `/api/v1/*` response (except the OpenAI-compatible chat
//! endpoint, which keeps its native shape) is wrapped in `{ "data": ... }`.
//! Handlers return [`Envelope<T>`] so the generated OpenAPI contract describes
//! the *actual* response body — `{ data: T }` — instead of the bare inner type.
//!
//! Errors use a separate `{ "error": { ... } }` shape (see [`crate::error`]).

use serde::Serialize;
use utoipa::ToSchema;

/// Success envelope wrapping a response payload under a `data` key.
///
/// `T` is the inner payload type (a single DTO, a `Vec<DTO>` for lists, or
/// `serde_json::Value` for ad-hoc acknowledgement bodies like `{ "ok": true }`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Envelope<T> {
    /// The response payload.
    pub data: T,
}

impl<T> Envelope<T> {
    /// Wrap a payload in the success envelope.
    pub fn new(data: T) -> Self {
        Self { data }
    }
}

impl<T> From<T> for Envelope<T> {
    fn from(data: T) -> Self {
        Self { data }
    }
}

/// List metadata accompanying a paginated response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Meta {
    /// Total number of records matching the query (before any limit/offset).
    pub total: u64,
}

/// Success envelope for list endpoints that report pagination metadata.
///
/// Wraps the records under `data` and a [`Meta`] block under `meta`, i.e.
/// `{ "data": [...], "meta": { "total": N } }`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ListEnvelope<T> {
    /// The page of records.
    pub data: Vec<T>,
    /// Pagination metadata.
    pub meta: Meta,
}

impl<T> ListEnvelope<T> {
    /// Wrap a page of records and its total count in the list envelope.
    pub fn new(data: Vec<T>, total: u64) -> Self {
        Self {
            data,
            meta: Meta { total },
        }
    }
}
