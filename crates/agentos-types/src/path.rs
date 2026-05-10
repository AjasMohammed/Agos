use std::path::{Component, Path};

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("path traversal attempt detected")]
    Traversal,
}

/// Reject any path containing a parent-directory (`..`) component.
/// Returns the input string unchanged on success.
pub fn reject_traversal(s: &str) -> Result<&str, PathError> {
    for component in Path::new(s).components() {
        if matches!(component, Component::ParentDir) {
            return Err(PathError::Traversal);
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_normal_path() {
        assert!(reject_traversal("foo/bar/baz.txt").is_ok());
    }

    #[test]
    fn accept_absolute_path() {
        assert!(reject_traversal("/opt/agentos/data/notes.md").is_ok());
    }

    #[test]
    fn reject_dotdot() {
        assert!(reject_traversal("../../etc/passwd").is_err());
    }

    #[test]
    fn reject_dotdot_in_middle() {
        assert!(reject_traversal("foo/../bar").is_err());
    }

    #[test]
    fn accept_single_dot() {
        // A single `.` (CurDir) is fine — it is not a parent traversal.
        assert!(reject_traversal("./notes.txt").is_ok());
    }
}
