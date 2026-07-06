//! End-to-end integration tests for the AgentOS kernel.
//!
//! These tests boot a real kernel against a temp directory, connect via the
//! Unix domain socket bus, and exercise full request/response flows.
//!
//! Run with:
//!   cargo test -p agentos-kernel --test e2e

#[path = "e2e/common.rs"]
mod common;

#[path = "e2e/kernel_boot.rs"]
mod kernel_boot;

#[path = "e2e/shutdown_audit.rs"]
mod shutdown_audit;

#[path = "e2e/agent_identity.rs"]
mod agent_identity;

#[path = "e2e/chat_tool_loop.rs"]
mod chat_tool_loop;

#[path = "e2e/multimodal_chat.rs"]
mod multimodal_chat;

#[path = "e2e/chat_manifest_selection.rs"]
mod chat_manifest_selection;

#[path = "e2e/native_tool_call_round_trip.rs"]
mod native_tool_call_round_trip;

#[path = "e2e/workspace_grant_e2e.rs"]
mod workspace_grant_e2e;

#[path = "e2e/approval_modes_e2e.rs"]
mod approval_modes_e2e;

#[path = "e2e/event_trigger_e2e.rs"]
mod event_trigger_e2e;

#[path = "e2e/personalization.rs"]
mod personalization;

#[path = "e2e/task_checkout_e2e.rs"]
mod task_checkout_e2e;

#[path = "e2e/heartbeat_work_loop_e2e.rs"]
mod heartbeat_work_loop_e2e;
