use serde_json::Value;
use std::collections::HashMap;

/// Registry that maps tool/schema names to compiled JSON Schema validators.
/// Populated from `ToolManifest.input_schema` during tool registration.
pub struct SchemaRegistry {
    schemas: HashMap<String, Value>,
}

impl SchemaRegistry {
    pub fn new() -> Self {
        Self {
            schemas: HashMap::new(),
        }
    }

    /// Register a JSON Schema for a given tool/schema name.
    pub fn register(&mut self, name: &str, schema: Value) {
        self.schemas.insert(name.to_string(), schema);
    }

    /// Validate a payload against the schema registered for `schema_name`.
    /// Returns `Ok(())` if no schema is registered (permissive by default)
    /// or if the payload passes validation.
    pub fn validate(&self, schema_name: &str, payload: &Value) -> Result<(), String> {
        let schema = match self.schemas.get(schema_name) {
            Some(s) => s,
            None => return Ok(()), // No schema registered — allow through
        };

        let validator = jsonschema::validator_for(schema).map_err(|e| {
            format!(
                "Invalid JSON Schema for '{}': {}",
                schema_name, e
            )
        })?;

        let errors: Vec<String> = validator
            .iter_errors(payload)
            .map(|e| format!("{} at {}", e, e.instance_path))
            .collect();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Schema validation failed for '{}': {}",
                schema_name,
                errors.join("; ")
            ))
        }
    }

    /// Check if a schema is registered for the given name.
    pub fn has_schema(&self, name: &str) -> bool {
        self.schemas.contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_no_schema_passes() {
        let registry = SchemaRegistry::new();
        let result = registry.validate("unknown", &json!({"anything": true}));
        assert!(result.is_ok());
    }

    #[test]
    fn test_valid_payload_passes() {
        let mut registry = SchemaRegistry::new();
        registry.register(
            "FileReadIntent",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        );

        let result = registry.validate("FileReadIntent", &json!({"path": "/tmp/file.txt"}));
        assert!(result.is_ok());
    }

    #[test]
    fn test_invalid_payload_fails() {
        let mut registry = SchemaRegistry::new();
        registry.register(
            "FileReadIntent",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        );

        let result = registry.validate("FileReadIntent", &json!({"wrong_field": 123}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Schema validation failed"));
    }

    #[test]
    fn test_type_mismatch_fails() {
        let mut registry = SchemaRegistry::new();
        registry.register(
            "CountIntent",
            json!({
                "type": "object",
                "properties": {
                    "count": { "type": "integer" }
                },
                "required": ["count"]
            }),
        );

        let result = registry.validate("CountIntent", &json!({"count": "not_a_number"}));
        assert!(result.is_err());
    }
}
