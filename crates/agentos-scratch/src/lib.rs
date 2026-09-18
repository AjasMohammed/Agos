//! **Experimental.** Agent scratchpad store. Schema and API may change before v2; not covered by the v1 stability promise.

pub mod error;
pub mod graph;
pub mod links;
pub mod store;
pub mod types;

pub use error::ScratchError;
pub use graph::{GraphWalker, SubgraphResult};
pub use links::{parse_wikilinks, WikiLink};
pub use store::ScratchpadStore;
pub use types::{
    parse_page_ref, LinkInfo, OutlinkInfo, PageRef, PageSummary, ScratchPage, SearchResult,
};
