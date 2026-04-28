use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeManifestError {
    #[error("failed to read node manifest file '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse node manifest '{path}': {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("node manifest '{id}' is missing required field: {field}")]
    MissingField { id: String, field: &'static str },
}
