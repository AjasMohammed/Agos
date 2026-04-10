use regex::Regex;

/// Scans text for leaked credentials and replaces them with `[REDACTED]`.
///
/// Used as a defense-in-depth layer: even if a token accidentally enters the
/// LLM context window (e.g., via a tool output), the redactor strips it before
/// it reaches the model.
pub struct ContextRedactor {
    patterns: Vec<Regex>,
}

impl Default for ContextRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextRedactor {
    pub fn new() -> Self {
        // These patterns are intentionally broad — false positives (replacing a
        // non-sensitive string that happens to look like a token) are far less
        // costly than a false negative (letting a real token through).
        let patterns = vec![
            // "Bearer <token>" in any context
            Regex::new(r"Bearer\s+[A-Za-z0-9\-._~+/]{20,}=*").expect("known-valid regex"),
            // GitHub personal access tokens (classic and fine-grained)
            Regex::new(r"gh[ps]_[A-Za-z0-9]{36,}").expect("known-valid regex"),
            // GitHub OAuth tokens
            Regex::new(r"gho_[A-Za-z0-9]{36,}").expect("known-valid regex"),
            // OpenAI / Stripe secret keys
            Regex::new(r"sk-[A-Za-z0-9]{32,}").expect("known-valid regex"),
            // Slack bot/user tokens
            Regex::new(r"xox[bpas]-[A-Za-z0-9\-]+").expect("known-valid regex"),
            // Generic "token": "..." or token = "..." patterns with long values
            Regex::new(r#"(?i)(?:token|secret|api_key|apikey|authorization)[\"']?\s*[:=]\s*[\"'][A-Za-z0-9\-._~+/]{20,}[\"']"#).expect("known-valid regex"),
        ];

        Self { patterns }
    }

    /// Redact any token-like patterns in the input text.
    pub fn redact(&self, text: &str) -> String {
        let mut result = text.to_string();
        for pattern in &self.patterns {
            result = pattern.replace_all(&result, "[REDACTED]").to_string();
        }
        result
    }

    /// Returns `true` if the text contains any token-like patterns.
    pub fn contains_sensitive(&self, text: &str) -> bool {
        self.patterns.iter().any(|p| p.is_match(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bearer_token() {
        let r = ContextRedactor::new();
        let input = "Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.long.token";
        let output = r.redact(input);
        assert!(!output.contains("eyJ"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn test_github_pat() {
        let r = ContextRedactor::new();
        let input = "Using token ghp_1234567890abcdefABCDEF1234567890abcd for auth";
        let output = r.redact(input);
        assert!(!output.contains("ghp_"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn test_github_oauth() {
        let r = ContextRedactor::new();
        let input = "Access token: gho_abcdefghij1234567890abcdefghij123456";
        let output = r.redact(input);
        assert!(!output.contains("gho_"));
    }

    #[test]
    fn test_openai_key() {
        let r = ContextRedactor::new();
        let input = "OPENAI_API_KEY=sk-proj1234567890abcdefghijklmnopqrstuv";
        let output = r.redact(input);
        assert!(!output.contains("sk-"));
    }

    #[test]
    fn test_slack_token() {
        let r = ContextRedactor::new();
        let input = "Bot token: xoxb-123456789-abcdef-ghijklm";
        let output = r.redact(input);
        assert!(!output.contains("xoxb-"));
    }

    #[test]
    fn test_generic_token_field() {
        let r = ContextRedactor::new();
        let input = r#"{"api_key": "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6"}"#;
        let output = r.redact(input);
        assert!(!output.contains("a1b2c3d4"));
    }

    #[test]
    fn test_non_sensitive_text_untouched() {
        let r = ContextRedactor::new();
        let input = "Hello world, this is a normal sentence with no tokens.";
        let output = r.redact(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_short_values_not_redacted() {
        let r = ContextRedactor::new();
        // "Bearer abc" is too short (< 20 chars) to be a real token
        let input = "Authorization: Bearer abc";
        let output = r.redact(input);
        assert_eq!(output, input);
    }

    #[test]
    fn test_contains_sensitive() {
        let r = ContextRedactor::new();
        assert!(r.contains_sensitive("token ghp_1234567890abcdefABCDEF1234567890abcd"));
        assert!(!r.contains_sensitive("normal text"));
    }

    #[test]
    fn test_multiple_patterns_in_one_string() {
        let r = ContextRedactor::new();
        let input = "Bearer eyJabc123456789012345678 and also sk-abcdefghijklmnop12345678901234567";
        let output = r.redact(input);
        assert!(!output.contains("eyJ"));
        assert!(!output.contains("sk-"));
        // Should have two [REDACTED] markers
        assert_eq!(output.matches("[REDACTED]").count(), 2);
    }
}
