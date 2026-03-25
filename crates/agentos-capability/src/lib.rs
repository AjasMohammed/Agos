pub mod engine;
pub mod permissions;
pub mod profiles;
pub mod token;

pub use engine::CapabilityEngine;
pub use permissions::parse_permission_str;
pub use profiles::{PermissionProfile, ProfileManager};
pub use token::{compute_signature, verify_token_signature};

/// Permission resource string: send a fire-and-forget notification to the user.
/// Requires the `write` bit on this resource.
pub const PERM_USER_NOTIFY: &str = "user.notify";

/// Permission resource string: send a blocking interactive question to the user.
/// Requires the `execute` bit on this resource.
pub const PERM_USER_INTERACT: &str = "user.interact";

/// Permission resource string: read/observe notification delivery status.
pub const PERM_USER_STATUS: &str = "user.status";
