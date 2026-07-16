use agentos_types::tool::PayloadExample;
use agentos_types::{AgentOSError, TrustTier};
use serde_json::Value;
use std::collections::HashMap;

/// Registry that maps tool names to compiled JSON Schema validators.
///
/// Populated from `ToolManifest.payload_schema` during tool registration. The
/// registry also remembers each tool's `TrustTier` so the pre-dispatch
/// validator can fail-closed on `Core`/`Verified` manifests (kernel-shipped or
/// co-signed; a validation failure is a programming error) and fail-open on
/// `Community` manifests where untrusted-author schemas may drift from the
/// runtime's actual deserializer.
pub struct SchemaRegistry {
    schemas: HashMap<String, SchemaEntry>,
}

struct SchemaEntry {
    #[allow(dead_code)]
    raw: Value,
    compiled: jsonschema::Validator,
    trust_tier: TrustTier,
}

impl SchemaRegistry {
    pub fn new() -> Self {
        Self {
            schemas: HashMap::new(),
        }
    }

    /// Register a JSON Schema for a given tool name with no associated trust
    /// tier metadata. Behaves as `Community` for fail-open purposes.
    ///
    /// Returns `AgentOSError::ToolSchemaInvalid` if the schema does not compile.
    pub fn register(&mut self, name: &str, schema: Value) -> Result<(), AgentOSError> {
        self.register_with_tier(name, schema, TrustTier::Community, &[])
    }

    /// Register a JSON Schema for a tool, carrying its trust tier and worked
    /// examples. Examples are validated against the compiled schema before
    /// the entry is inserted — drift between schema and example is a loud
    /// boot failure rather than a silent corruption.
    ///
    /// Returns `AgentOSError::ToolSchemaInvalid` for compilation failures or
    /// for any example that does not validate against the schema.
    pub fn register_with_tier(
        &mut self,
        name: &str,
        schema: Value,
        trust_tier: TrustTier,
        examples: &[PayloadExample],
    ) -> Result<(), AgentOSError> {
        let compiled =
            jsonschema::validator_for(&schema).map_err(|e| AgentOSError::ToolSchemaInvalid {
                name: name.to_string(),
                reason: e.to_string(),
            })?;

        for (idx, example) in examples.iter().enumerate() {
            if let Some(err) = compiled.iter_errors(&example.payload).next() {
                return Err(AgentOSError::ToolSchemaInvalid {
                    name: name.to_string(),
                    reason: format!(
                        "example[{idx}] does not validate against schema: {} - {}",
                        normalize_pointer(&err.instance_path.to_string()),
                        err
                    ),
                });
            }
        }

        self.schemas.insert(
            name.to_string(),
            SchemaEntry {
                raw: schema,
                compiled,
                trust_tier,
            },
        );
        Ok(())
    }

    /// Validate a payload against the schema registered for `tool_name`.
    ///
    /// Returns `Ok(())` when no schema is registered (fail-open for tools
    /// without a declared schema — current behavior preserved during migration).
    ///
    /// For schemas tagged `Core`/`Verified` (kernel-shipped or co-signed), a
    /// validation failure returns a structured `AgentOSError::ToolPayloadValidationFailed`
    /// with an RFC 6901 JSON Pointer to the offending field — fail-closed.
    ///
    /// For schemas tagged `Community`, validation failures are *also* surfaced,
    /// but the caller decides whether to enforce. The current task executor
    /// enforces uniformly; callers that want fail-open Community behavior can
    /// use `validate_for_dispatch` instead.
    pub fn validate(&self, tool_name: &str, payload: &Value) -> Result<(), AgentOSError> {
        let entry = match self.schemas.get(tool_name) {
            Some(s) => s,
            None => return Ok(()),
        };

        if let Some(err) = entry.compiled.iter_errors(payload).next() {
            Err(AgentOSError::ToolPayloadValidationFailed {
                tool_name: tool_name.to_string(),
                pointer: normalize_pointer(&err.instance_path.to_string()),
                reason: err.to_string(),
            })
        } else {
            Ok(())
        }
    }

    /// Pre-dispatch validation with trust-tier awareness.
    ///
    /// - `Core`/`Verified`: fail-closed. A validation failure is a structured
    ///   `ToolPayloadValidationFailed` error.
    /// - `Community`/`Blocked` (or unknown): fail-open. The validator returns
    ///   `Ok(())` for malformed payloads — the tool's own deserializer is the
    ///   authoritative gate, since the schema may not match the actual Rust
    ///   type for unaudited authors.
    /// - No schema registered: `Ok(())` (current behavior).
    ///
    /// Returns `(Ok, Some(soft_diagnostic))` when validation was soft-failed —
    /// callers may surface the diagnostic to the agent as a hint without
    /// aborting dispatch.
    pub fn validate_for_dispatch(
        &self,
        tool_name: &str,
        payload: &Value,
    ) -> Result<Option<String>, AgentOSError> {
        let entry = match self.schemas.get(tool_name) {
            Some(s) => s,
            None => return Ok(None),
        };

        let first_err = match entry.compiled.iter_errors(payload).next() {
            Some(e) => e,
            None => return Ok(None),
        };

        let pointer = normalize_pointer(&first_err.instance_path.to_string());
        let reason = first_err.to_string();

        match entry.trust_tier {
            TrustTier::Core | TrustTier::Verified => {
                Err(AgentOSError::ToolPayloadValidationFailed {
                    tool_name: tool_name.to_string(),
                    pointer,
                    reason,
                })
            }
            TrustTier::Community | TrustTier::Blocked => Ok(Some(format!(
                "tool '{tool_name}': payload schema mismatch (soft): {pointer} - {reason}"
            ))),
        }
    }

    /// Check if a schema is registered for the given name.
    pub fn has_schema(&self, name: &str) -> bool {
        self.schemas.contains_key(name)
    }
}

fn normalize_pointer(p: &str) -> String {
    if p.is_empty() {
        "/".to_string()
    } else if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

impl Default for SchemaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::tool::PayloadExample;
    use serde_json::json;

    fn person_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        })
    }

    #[test]
    fn no_schema_passes() {
        let registry = SchemaRegistry::new();
        assert!(registry
            .validate("unknown", &json!({"anything": true}))
            .is_ok());
        assert!(registry
            .validate_for_dispatch("unknown", &json!({"anything": true}))
            .unwrap()
            .is_none());
    }

    #[test]
    fn valid_payload_passes() {
        let mut registry = SchemaRegistry::new();
        registry
            .register_with_tier("file-read", person_schema(), TrustTier::Core, &[])
            .expect("schema should compile");
        assert!(registry
            .validate("file-read", &json!({"path": "/tmp/x"}))
            .is_ok());
        assert!(registry
            .validate_for_dispatch("file-read", &json!({"path": "/tmp/x"}))
            .unwrap()
            .is_none());
    }

    #[test]
    fn invalid_payload_fails_for_core() {
        let mut registry = SchemaRegistry::new();
        registry
            .register_with_tier("file-read", person_schema(), TrustTier::Core, &[])
            .expect("schema should compile");
        let err = registry
            .validate_for_dispatch("file-read", &json!({"wrong": 1}))
            .unwrap_err();
        match err {
            AgentOSError::ToolPayloadValidationFailed {
                tool_name, pointer, ..
            } => {
                assert_eq!(tool_name, "file-read");
                assert!(pointer.starts_with('/'));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn invalid_payload_soft_fails_for_community() {
        let mut registry = SchemaRegistry::new();
        registry
            .register_with_tier("rando-tool", person_schema(), TrustTier::Community, &[])
            .expect("schema should compile");
        let diag = registry
            .validate_for_dispatch("rando-tool", &json!({"wrong": 1}))
            .expect("community tier must soft-fail (Ok)");
        assert!(diag.is_some(), "should produce a soft diagnostic");
        let msg = diag.unwrap();
        assert!(msg.contains("rando-tool"));
        assert!(msg.contains("soft"));
    }

    #[test]
    fn invalid_schema_returns_tool_schema_invalid() {
        let mut registry = SchemaRegistry::new();
        let bad_schema = json!({"type": "not-a-real-type"});
        let err = registry.register("broken", bad_schema).unwrap_err();
        assert!(matches!(err, AgentOSError::ToolSchemaInvalid { .. }));
    }

    #[test]
    fn example_validated_against_schema_at_load() {
        let mut registry = SchemaRegistry::new();
        let bad_example = PayloadExample {
            description: Some("broken".into()),
            payload: json!({"wrong_field": 1}),
        };
        let err = registry
            .register_with_tier(
                "file-read",
                person_schema(),
                TrustTier::Core,
                &[bad_example],
            )
            .unwrap_err();
        match err {
            AgentOSError::ToolSchemaInvalid { name, reason } => {
                assert_eq!(name, "file-read");
                assert!(reason.contains("example[0]"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn valid_example_passes_at_load() {
        let mut registry = SchemaRegistry::new();
        let good_example = PayloadExample {
            description: None,
            payload: json!({"path": "/tmp/x"}),
        };
        registry
            .register_with_tier(
                "file-read",
                person_schema(),
                TrustTier::Core,
                &[good_example],
            )
            .expect("good example must pass");
    }

    #[test]
    fn pointer_is_normalised() {
        let mut registry = SchemaRegistry::new();
        registry
            .register_with_tier(
                "must-be-int",
                json!({
                    "type": "object",
                    "properties": { "count": { "type": "integer" } },
                    "required": ["count"]
                }),
                TrustTier::Core,
                &[],
            )
            .expect("schema should compile");

        // Wrong type → pointer points to /count
        let err = registry
            .validate_for_dispatch("must-be-int", &json!({"count": "not-a-num"}))
            .unwrap_err();
        if let AgentOSError::ToolPayloadValidationFailed { pointer, .. } = err {
            assert!(pointer.starts_with('/'), "pointer must begin with '/'");
        } else {
            panic!("wrong variant");
        }
    }
}
