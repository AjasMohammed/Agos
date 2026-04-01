//! HTTP handler modules — one per resource area.
//!
//! Each handler function takes Axum extractors (Path, Query, Json, State) and
//! delegates to the [`KernelService`] trait. Errors are returned as [`ApiError`]
//! which implements `IntoResponse`.

pub mod agents;
pub mod audit;
pub mod chat;
pub mod costs;
pub mod notifications;
pub mod pipelines;
pub mod secrets;
pub mod system;
pub mod tasks;
pub mod tools;
