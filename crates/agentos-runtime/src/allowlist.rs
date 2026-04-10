use std::collections::HashSet;

/// Operator-defined list of Docker images that agents are allowed to use.
///
/// Agents cannot pull arbitrary images — only those pre-approved by the
/// operator. This prevents supply-chain attacks via malicious base images.
pub struct ImageAllowlist {
    allowed: HashSet<String>,
}

impl ImageAllowlist {
    pub fn new(images: Vec<String>) -> Self {
        Self {
            allowed: images.into_iter().collect(),
        }
    }

    /// Check if an image is in the allowlist.
    ///
    /// Matches exactly — "python:3.11-slim" does NOT match "python:3.11".
    pub fn is_allowed(&self, image: &str) -> bool {
        self.allowed.contains(image)
    }

    /// Return the number of allowed images.
    pub fn len(&self) -> usize {
        self.allowed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_image() {
        let list = ImageAllowlist::new(vec!["python:3.11-slim".into(), "node:20-alpine".into()]);
        assert!(list.is_allowed("python:3.11-slim"));
        assert!(list.is_allowed("node:20-alpine"));
        assert!(!list.is_allowed("python:3.12-slim"));
        assert!(!list.is_allowed("ubuntu:22.04"));
    }

    #[test]
    fn test_empty_allowlist() {
        let list = ImageAllowlist::new(vec![]);
        assert!(!list.is_allowed("python:3.11-slim"));
        assert!(list.is_empty());
    }
}
