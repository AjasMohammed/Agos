//! DTOs for the visual-workflow CRUD surface.
//!
//! Workflows are stored as JSON documents under `<data_dir>/workflows/<id>.json`.
//! The full body is round-tripped as an opaque `serde_json::Value` so the React
//! editor owns the schema; the API only validates structural basics and node
//! counts. Workflow *execution* is intentionally out of scope (no live
//! `NodeRegistry` is built at kernel boot today).

use serde::{Deserialize, Serialize};

/// A workflow as listed by `GET /api/v1/workflows`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiWorkflowSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub node_count: usize,
    /// Currently always `saved`; reserved for future lifecycle states.
    pub status: String,
}

/// Request body for `POST /api/v1/workflows` and `PUT /api/v1/workflows/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SaveWorkflowRequest {
    pub name: String,
    /// Full workflow definition (nodes, connections, settings). Opaque to the API.
    pub definition: serde_json::Value,
}

/// Response for workflow create/update — echoes the resolved id.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkflowSaveResponse {
    pub id: String,
}
