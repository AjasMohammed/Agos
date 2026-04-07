/// Supported payload transform operations for fallback chains.
///
/// Transform strings use "op:value" format, e.g., "prepend:/tmp/".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransformOp {
    /// Prepend a string to the value.
    Prepend(String),
    /// Append a string to the value.
    Append(String),
    /// Replace the entire value.
    Replace(String),
    /// Set a default if the key is missing.
    Default(String),
}

impl TransformOp {
    /// Parse a transform string like "prepend:/tmp/" into an operation.
    pub fn parse(s: &str) -> Result<Self, String> {
        let (op, value) = s
            .split_once(':')
            .ok_or_else(|| format!("Invalid transform syntax (expected 'op:value'): {s}"))?;
        match op {
            "prepend" => Ok(Self::Prepend(value.to_string())),
            "append" => Ok(Self::Append(value.to_string())),
            "replace" => Ok(Self::Replace(value.to_string())),
            "default" => Ok(Self::Default(value.to_string())),
            _ => Err(format!("Unknown transform operation: {op}")),
        }
    }

    /// Apply this transform to a JSON string value.
    pub fn apply(&self, current: Option<&str>) -> String {
        match self {
            Self::Prepend(prefix) => format!("{}{}", prefix, current.unwrap_or("")),
            Self::Append(suffix) => format!("{}{}", current.unwrap_or(""), suffix),
            Self::Replace(new_val) => new_val.clone(),
            Self::Default(default) => current.unwrap_or(default).to_string(),
        }
    }
}

/// Apply a set of named transforms to a JSON payload.
///
/// Each entry in `transforms` maps a payload key to a transform string
/// (e.g., `"path" → "prepend:/tmp/"`). Keys that don't exist are created
/// by `Replace` and `Default` operations.
pub fn apply_transforms(
    payload: &serde_json::Value,
    transforms: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
    let mut result = payload.clone();
    for (key, transform_str) in transforms {
        if let Ok(op) = TransformOp::parse(transform_str) {
            let current = result.get(key).and_then(|v| v.as_str());
            let new_value = op.apply(current);
            if let Some(obj) = result.as_object_mut() {
                obj.insert(key.clone(), serde_json::json!(new_value));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_prepend() {
        assert_eq!(
            TransformOp::parse("prepend:/tmp/").unwrap(),
            TransformOp::Prepend("/tmp/".into())
        );
    }

    #[test]
    fn parse_append() {
        assert_eq!(
            TransformOp::parse("append:.bak").unwrap(),
            TransformOp::Append(".bak".into())
        );
    }

    #[test]
    fn parse_replace() {
        assert_eq!(
            TransformOp::parse("replace:new_value").unwrap(),
            TransformOp::Replace("new_value".into())
        );
    }

    #[test]
    fn parse_default() {
        assert_eq!(
            TransformOp::parse("default:fallback").unwrap(),
            TransformOp::Default("fallback".into())
        );
    }

    #[test]
    fn parse_unknown_op() {
        assert!(TransformOp::parse("truncate:5").is_err());
    }

    #[test]
    fn parse_no_colon() {
        assert!(TransformOp::parse("nocolon").is_err());
    }

    #[test]
    fn apply_prepend_with_value() {
        let op = TransformOp::Prepend("/tmp/".into());
        assert_eq!(op.apply(Some("file.txt")), "/tmp/file.txt");
    }

    #[test]
    fn apply_prepend_without_value() {
        let op = TransformOp::Prepend("/tmp/".into());
        assert_eq!(op.apply(None), "/tmp/");
    }

    #[test]
    fn apply_append_with_value() {
        let op = TransformOp::Append(".bak".into());
        assert_eq!(op.apply(Some("file.txt")), "file.txt.bak");
    }

    #[test]
    fn apply_replace() {
        let op = TransformOp::Replace("new".into());
        assert_eq!(op.apply(Some("old")), "new");
        assert_eq!(op.apply(None), "new");
    }

    #[test]
    fn apply_default_with_value() {
        let op = TransformOp::Default("fallback".into());
        assert_eq!(op.apply(Some("existing")), "existing");
    }

    #[test]
    fn apply_default_without_value() {
        let op = TransformOp::Default("fallback".into());
        assert_eq!(op.apply(None), "fallback");
    }

    #[test]
    fn apply_transforms_to_payload() {
        let payload = serde_json::json!({
            "path": "file.txt",
            "mode": "write"
        });
        let mut transforms = std::collections::HashMap::new();
        transforms.insert("path".into(), "prepend:/tmp/overflow/".into());

        let result = apply_transforms(&payload, &transforms);
        assert_eq!(result["path"], "/tmp/overflow/file.txt");
        assert_eq!(result["mode"], "write"); // untouched
    }

    #[test]
    fn apply_transforms_creates_missing_key() {
        let payload = serde_json::json!({"path": "file.txt"});
        let mut transforms = std::collections::HashMap::new();
        transforms.insert("timeout_ms".into(), "default:30000".into());

        let result = apply_transforms(&payload, &transforms);
        assert_eq!(result["timeout_ms"], "30000");
    }
}
