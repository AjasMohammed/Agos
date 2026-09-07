pub mod auth;
pub mod chat_inflight;
// ChatStore + ConvoStore relocated into agentos-kernel (shared with the REST API);
// re-exported so existing `crate::chat_store::…` / `crate::convo_store::…` paths resolve.
pub use agentos_kernel::chat_store;
pub mod convo_inflight;
pub use agentos_kernel::convo_store;
pub mod csrf;
// `FileStore` was relocated into `agentos-kernel` so the REST API (`KernelService`)
// and the web UI share one instance. Re-exported here so existing
// `crate::file_store::…` paths in this crate keep resolving.
pub use agentos_kernel::file_store;
pub mod handlers;
pub mod router;
pub mod server;
pub mod state;
pub mod templates;

pub use server::WebServer;
pub use state::AppState;
