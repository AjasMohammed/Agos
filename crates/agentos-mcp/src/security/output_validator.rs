use serde_json::Value;

/// Validates and sanitizes MCP tool output before it reaches the agent.
pub struct OutputValidator {
    /// Default max response size in bytes. Can be overridden per server.
    max_response_bytes: usize,
}

impl OutputValidator {
    pub fn new(max_response_bytes: usize) -> Self {
        Self { max_response_bytes }
    }

    /// Validate a JSON-RPC result value.
    ///
    /// Checks:
    /// - Size limit (truncate if exceeds limit)
    /// - Depth limit (max 32 levels of nesting)
    /// - Base64 payloads (reject if >100KB)
    ///
    /// Returns the validated value (possibly truncated) or an error.
    pub fn validate(
        &self,
        value: &Value,
        server_max_bytes: Option<usize>,
    ) -> Result<Value, String> {
        let max_bytes = server_max_bytes.unwrap_or(self.max_response_bytes);

        // Estimate JSON-encoded size.
        let encoded = serde_json::to_string(value)
            .map_err(|e| format!("Failed to serialize output: {}", e))?;
        let size = encoded.len();

        if size > max_bytes {
            // Find a safe UTF-8 char boundary for truncation.
            let target = encoded.len().min(max_bytes.saturating_sub(100));
            let mut safe_end = target.min(encoded.len());
            while safe_end > 0 && !encoded.is_char_boundary(safe_end) {
                safe_end -= 1;
            }
            let truncated = format!(
                "{}...[truncated: original size was {} bytes]",
                &encoded[..safe_end],
                size
            );
            return Ok(Value::String(truncated));
        }

        // Check max nesting depth.
        if self.max_depth(value) > 32 {
            return Err("JSON response exceeds max nesting depth (32)".into());
        }

        // Check for suspicious base64 blobs.
        if let Some(blob_size) = self.detect_large_base64(value) {
            if blob_size > 100 * 1024 {
                return Err(format!(
                    "Base64 payload exceeds 100KB limit: {} bytes",
                    blob_size
                ));
            }
        }

        Ok(value.clone())
    }

    /// Calculate maximum nesting depth of a JSON value.
    fn max_depth(&self, value: &Value) -> u32 {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 1,
            Value::Array(arr) => 1 + arr.iter().map(|v| self.max_depth(v)).max().unwrap_or(0),
            Value::Object(obj) => 1 + obj.values().map(|v| self.max_depth(v)).max().unwrap_or(0),
        }
    }

    /// Detect large base64 strings (potential binary data).
    /// Returns the estimated size if found, or None.
    fn detect_large_base64(&self, value: &Value) -> Option<usize> {
        match value {
            Value::String(s) => {
                // Heuristic: strings >1KB matching base64 charset are likely binary.
                if s.len() > 1000 && is_likely_base64(s) {
                    Some(s.len())
                } else {
                    None
                }
            }
            Value::Array(arr) => arr.iter().filter_map(|v| self.detect_large_base64(v)).max(),
            Value::Object(obj) => obj
                .values()
                .filter_map(|v| self.detect_large_base64(v))
                .max(),
            _ => None,
        }
    }
}

fn is_likely_base64(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_small_object_succeeds() {
        let validator = OutputValidator::new(1024);
        let value = serde_json::json!({"ok": true});
        let result = validator.validate(&value, None);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_exceeds_size_truncates() {
        let validator = OutputValidator::new(50);
        let value = serde_json::json!({"message": "this is a very long string that exceeds the size limit"});
        let result = validator.validate(&value, None).unwrap();
        let s = result.as_str().unwrap();
        assert!(s.contains("truncated"));
        assert!(s.contains("original size was"));
    }

    #[test]
    fn validate_too_deep_rejects() {
        let validator = OutputValidator::new(10000);
        // Build a deeply nested structure.
        let mut value = Value::Number(1.into());
        for _ in 0..35 {
            value = serde_json::json!({ "nested": value });
        }
        let result = validator.validate(&value, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nesting depth"));
    }

    #[test]
    fn validate_rejects_large_base64() {
        let validator = OutputValidator::new(200000);
        let large_b64 = "A".repeat(101 * 1024); // 101 KB of 'A's (looks like base64)
        let value = serde_json::json!({"data": large_b64});
        let result = validator.validate(&value, None);
        assert!(result.is_err());
    }

    #[test]
    fn is_likely_base64_detects() {
        assert!(is_likely_base64("SGVsbG8gV29ybGQ=")); // "Hello World" in base64
        assert!(is_likely_base64("AAAA++++////")); // with + and /
        assert!(!is_likely_base64("Hello World!")); // has ! which isn't base64
    }
}
