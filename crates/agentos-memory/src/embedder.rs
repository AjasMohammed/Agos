use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::Path;

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    /// Downloads and initializes the embedding model during construction (~23MB for MiniLM).
    pub fn new() -> Result<Self, anyhow::Error> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(true),
        )?;
        Ok(Self { model })
    }

    /// Downloads and initializes the embedding model with an explicit cache directory.
    pub fn with_cache_dir(cache_dir: &Path) -> Result<Self, anyhow::Error> {
        std::fs::create_dir_all(cache_dir)?;
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_show_download_progress(true)
                .with_cache_dir(cache_dir.to_path_buf()),
        )?;
        Ok(Self { model })
    }

    /// Embed one or many texts — batched for efficiency.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, anyhow::Error> {
        self.model.embed(texts.to_vec(), None)
    }

    /// Helper string chunker for long documents
    pub fn chunk_text(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut start_idx = 0;

        // Ensure valid parameters
        if chunk_size == 0 {
            return vec![text.to_string()];
        }

        let safe_overlap = if overlap >= chunk_size {
            chunk_size / 2
        } else {
            overlap
        };

        while start_idx < text.len() {
            let mut end_idx = start_idx + chunk_size;

            // Don't chunk in the middle of a boundary char if we can avoid it
            if end_idx < text.len() {
                // Find nearest whitespace backwards
                while end_idx > start_idx && !text.is_char_boundary(end_idx) {
                    end_idx -= 1;
                }

                // If end_idx collapsed to start_idx (e.g. chunk_size < char width),
                // advance to next valid char boundary
                if end_idx == start_idx {
                    end_idx = start_idx + 1;
                    while end_idx < text.len() && !text.is_char_boundary(end_idx) {
                        end_idx += 1;
                    }
                }

                let maybe_space = text[start_idx..end_idx].rfind(char::is_whitespace);
                if let Some(space_idx) = maybe_space {
                    // Only use nearest space if it isn't too far back
                    if space_idx > chunk_size / 2 {
                        end_idx = start_idx + space_idx;
                    }
                }
            } else {
                end_idx = text.len();
            }

            if end_idx > start_idx {
                chunks.push(text[start_idx..end_idx].to_string());
            }

            if end_idx == text.len() {
                break;
            }

            // Move forward, minus the overlap
            start_idx = end_idx.saturating_sub(safe_overlap);
            // Must step forward to avoid infinite loop
            if start_idx == end_idx {
                start_idx += 1;
            }
            // Align to character boundary
            while start_idx < text.len() && !text.is_char_boundary(start_idx) {
                start_idx += 1;
            }
        }

        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_text() {
        let text = "This is a simple test that should be split into a few chunks.";
        let chunks = Embedder::chunk_text(text, 20, 5);

        // E.g. "This is a simple", "ple test that s", "t should be spli", ...
        assert!(!chunks.is_empty());
        assert!(chunks[0].starts_with("This is a simple"));
    }

    #[test]
    fn test_embed_single_text_returns_correct_dimension() {
        let embedder = Embedder::new().unwrap();
        let vecs = embedder.embed(&["hello world"]).unwrap();
        assert_eq!(vecs[0].len(), 384); // MiniLM-L6-v2
    }
}
