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
