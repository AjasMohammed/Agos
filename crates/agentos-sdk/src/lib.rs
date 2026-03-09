//! AgentOS SDK — ergonomic macros and re-exports for building AgentOS tools.
//!
//! # Quick Start
//!
//! ```ignore
//! use agentos_sdk::prelude::*;
//!
//! #[tool(
//!     name = "my-tool",
//!     version = "1.0.0",
//!     description = "Does something useful",
//!     permissions = "fs.read:r"
//! )]
//! async fn my_tool(
//!     payload: serde_json::Value,
//!     context: ToolExecutionContext,
//! ) -> Result<serde_json::Value, AgentOSError> {
//!     Ok(serde_json::json!({"result": "done"}))
//! }
//! ```

pub use agentos_sdk_macros::tool;

/// Convenience prelude that imports everything needed for tool development.
pub mod prelude {
    pub use agentos_sdk_macros::tool;
    pub use agentos_tools::traits::{AgentTool, ToolExecutionContext};
    pub use agentos_types::{AgentOSError, PermissionOp};
    pub use async_trait::async_trait;
    pub use serde_json;
}
