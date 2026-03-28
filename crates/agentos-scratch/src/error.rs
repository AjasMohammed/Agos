use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScratchError {
    #[error("Page not found: {title} for agent {agent_id}")]
    PageNotFound { agent_id: String, title: String },

    #[error("Content too large: {size} bytes (max {max} bytes)")]
    ContentTooLarge { size: usize, max: usize },

    #[error("Too many pages for agent {agent_id}: {count} (max {max})")]
    TooManyPages {
        agent_id: String,
        count: usize,
        max: usize,
    },

    #[error("Title too long: {length} chars (max {max})")]
    TitleTooLong { length: usize, max: usize },

    #[error("Title cannot be empty")]
    EmptyTitle,

    #[error("Title contains invalid characters (control characters are not allowed)")]
    InvalidTitle,

    #[error("Search query cannot be empty")]
    EmptyQuery,

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}
