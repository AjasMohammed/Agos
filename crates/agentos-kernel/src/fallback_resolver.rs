use agentos_types::*;
use std::collections::HashSet;

/// Resolves tool failures through manifest-declared fallback chains.
///
/// When a tool fails, the resolver checks its manifest for `FallbackRule`s
/// that match the error category. If found, it transforms the payload and
/// attempts the fallback tool, repeating up to `max_chain_depth` hops.
///
/// The LLM only sees the final result. If all fallbacks fail, the original
/// error is returned to the LLM for reasoning.
pub struct FallbackResolver {
    max_chain_depth: u8,
}

/// Result of a successful fallback chain resolution.
pub struct FallbackResult {
    pub value: serde_json::Value,
    pub chain_length: u8,
}

impl FallbackResolver {
    pub fn new(max_chain_depth: u8) -> Self {
        Self {
            max_chain_depth: max_chain_depth.min(3),
        }
    }

    /// Attempt to resolve a tool failure through fallback chains.
    ///
    /// Returns `Some(result)` if a fallback succeeded, `None` if no
    /// fallback matched or all fallbacks also failed.
    pub async fn try_fallback<F, Fut>(
        &self,
        original_tool: &str,
        original_error: &AgentOSError,
        original_payload: &serde_json::Value,
        get_fallbacks: impl Fn(&str) -> Vec<FallbackRule>,
        execute_fn: F,
    ) -> Option<FallbackResult>
    where
        F: Fn(String, serde_json::Value) -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value, AgentOSError>>,
    {
        let error_cat = original_error.error_category();

        let fallbacks = get_fallbacks(original_tool);
        if fallbacks.is_empty() {
            return None;
        }

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(original_tool.to_string());

        let mut current_tool = original_tool.to_string();
        let mut current_payload = original_payload.clone();
        let mut current_error_cat = error_cat.to_string();
        let mut hop = 0u8;

        while hop < self.max_chain_depth {
            let rules = get_fallbacks(&current_tool);
            let rule = match rules.iter().find(|r| r.on_error == current_error_cat) {
                Some(r) => r.clone(),
                None => return None,
            };

            // Prevent cycles
            if visited.contains(&rule.try_tool) {
                tracing::warn!(
                    from_tool = %current_tool,
                    to_tool = %rule.try_tool,
                    "Fallback cycle detected, aborting chain"
                );
                return None;
            }
            visited.insert(rule.try_tool.clone());

            // Apply payload transforms
            let transformed = apply_transforms(&current_payload, &rule.transform);

            tracing::info!(
                from_tool = %current_tool,
                to_tool = %rule.try_tool,
                error_category = %current_error_cat,
                hop = hop,
                "Attempting fallback"
            );

            // Execute fallback
            match execute_fn(rule.try_tool.clone(), transformed.clone()).await {
                Ok(mut result) => {
                    // Tag the result so the LLM knows a fallback was used
                    if let Some(obj) = result.as_object_mut() {
                        obj.insert("_fallback_used".into(), serde_json::json!(true));
                        obj.insert("_original_tool".into(), serde_json::json!(original_tool));
                        obj.insert("_original_error".into(), serde_json::json!(error_cat));
                        obj.insert("_fallback_chain_length".into(), serde_json::json!(hop + 1));
                    }
                    tracing::info!(
                        original_tool = %original_tool,
                        fallback_tool = %rule.try_tool,
                        chain_length = hop + 1,
                        "Fallback succeeded"
                    );
                    return Some(FallbackResult {
                        value: result,
                        chain_length: hop + 1,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        fallback_tool = %rule.try_tool,
                        error = %e,
                        "Fallback also failed, continuing chain"
                    );
                    current_tool = rule.try_tool;
                    current_payload = transformed;
                    current_error_cat = e.error_category().to_string();
                    hop += 1;
                }
            }
        }

        tracing::warn!(
            original_tool = %original_tool,
            chain_depth = hop,
            "All fallbacks exhausted"
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rules(on_error: &str, try_tool: &str) -> Vec<FallbackRule> {
        vec![FallbackRule {
            on_error: on_error.to_string(),
            try_tool: try_tool.to_string(),
            transform: std::collections::HashMap::new(),
            max_retries: 1,
        }]
    }

    #[tokio::test]
    async fn single_fallback_succeeds() {
        let resolver = FallbackResolver::new(3);

        let result = resolver
            .try_fallback(
                "tool-a",
                &AgentOSError::StorageError("disk full".into()),
                &serde_json::json!({"path": "test.txt"}),
                |tool_name| {
                    if tool_name == "tool-a" {
                        make_rules("StorageError", "tool-b")
                    } else {
                        vec![]
                    }
                },
                |_tool_name, _payload| async { Ok(serde_json::json!({"status": "ok"})) },
            )
            .await;

        let result = result.expect("fallback should succeed");
        assert_eq!(result.chain_length, 1);
        assert_eq!(result.value["_fallback_used"], true);
        assert_eq!(result.value["_original_tool"], "tool-a");
    }

    #[tokio::test]
    async fn no_matching_fallback_returns_none() {
        let resolver = FallbackResolver::new(3);

        let result = resolver
            .try_fallback(
                "tool-a",
                &AgentOSError::StorageError("disk full".into()),
                &serde_json::json!({}),
                |_| make_rules("PermissionDenied", "tool-b"), // wrong error category
                |_tool_name, _payload| async { Ok(serde_json::json!({"status": "ok"})) },
            )
            .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn cycle_detection() {
        let resolver = FallbackResolver::new(3);

        let result = resolver
            .try_fallback(
                "tool-a",
                &AgentOSError::StorageError("fail".into()),
                &serde_json::json!({}),
                |tool_name| match tool_name {
                    "tool-a" => make_rules("StorageError", "tool-b"),
                    "tool-b" => make_rules("StorageError", "tool-a"), // cycle!
                    _ => vec![],
                },
                |_tool_name, _payload| async {
                    Err(AgentOSError::StorageError("still failing".into()))
                },
            )
            .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn max_depth_enforcement() {
        let resolver = FallbackResolver::new(2);

        // Chain: a -> b -> c -> d (should stop at depth 2)
        let result = resolver
            .try_fallback(
                "tool-a",
                &AgentOSError::StorageError("fail".into()),
                &serde_json::json!({}),
                |tool_name| match tool_name {
                    "tool-a" => make_rules("StorageError", "tool-b"),
                    "tool-b" => make_rules("StorageError", "tool-c"),
                    "tool-c" => make_rules("StorageError", "tool-d"),
                    _ => vec![],
                },
                |_tool_name, _payload| async {
                    Err(AgentOSError::StorageError("still failing".into()))
                },
            )
            .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn chained_fallback_succeeds_on_second_hop() {
        let resolver = FallbackResolver::new(3);

        let result = resolver
            .try_fallback(
                "tool-a",
                &AgentOSError::StorageError("fail".into()),
                &serde_json::json!({}),
                |tool_name| match tool_name {
                    "tool-a" => make_rules("StorageError", "tool-b"),
                    "tool-b" => make_rules("StorageError", "tool-c"),
                    _ => vec![],
                },
                |tool_name, _payload| async move {
                    if tool_name == "tool-c" {
                        Ok(serde_json::json!({"from": "tool-c"}))
                    } else {
                        Err(AgentOSError::StorageError("still failing".into()))
                    }
                },
            )
            .await;

        let result = result.expect("should succeed on second hop");
        assert_eq!(result.chain_length, 2);
        assert_eq!(result.value["_fallback_chain_length"], 2);
    }

    #[tokio::test]
    async fn empty_fallback_rules_returns_none() {
        let resolver = FallbackResolver::new(3);

        let result = resolver
            .try_fallback(
                "tool-a",
                &AgentOSError::StorageError("fail".into()),
                &serde_json::json!({}),
                |_| vec![], // no fallbacks declared
                |_tool_name, _payload| async { Ok(serde_json::json!({"status": "ok"})) },
            )
            .await;

        assert!(result.is_none());
    }
}
