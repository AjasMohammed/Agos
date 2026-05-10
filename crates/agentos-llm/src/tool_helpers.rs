//! Shared helpers for building tool payloads and parsing tool call responses
//! across LLM adapters (OpenAI, Anthropic, Gemini).

use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::OnceLock;
use tracing::{debug, warn};

/// Cache of `(schema_fingerprint, chosen_variant)` pairs we've already
/// warned about. Same (tool, schema, choice) triple recurs every time a
/// model context is rebuilt — without de-duplication the warning
/// drowns the log (~209 hits in a 3-hour session, observed 2026-05-08).
fn warn_once_cache() -> &'static Mutex<HashSet<u64>> {
    static CACHE: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn fingerprint(seed: u8, parts: &[&str]) -> u64 {
    let mut h = DefaultHasher::new();
    seed.hash(&mut h);
    for p in parts {
        p.hash(&mut h);
    }
    h.finish()
}

/// Soft cap on the de-dup cache. A misbehaving MCP server emitting
/// unique-per-call schemas could otherwise grow the set unbounded.
/// 4096 is comfortably above any realistic tool catalogue (~200 tools
/// × a few schema variants each) but still bounded.
const WARN_ONCE_CACHE_CAP: usize = 4096;

/// Returns true exactly once per process for a given fingerprint. If
/// the cache hits its cap (suggesting a stream of unique schemas), we
/// drain it and start over — the next batch of warnings will fire
/// fresh, which is the right signal for "something is generating
/// unbounded schema variants".
fn warn_once(fp: u64) -> bool {
    let mut guard = match warn_once_cache().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.len() >= WARN_ONCE_CACHE_CAP {
        guard.clear();
    }
    guard.insert(fp)
}

/// Rank for picking the most-informative variant when a tool schema
/// declares a multi-type union (e.g. `"type": ["string", "array"]`).
/// Higher rank wins — array/object preserve structural information that
/// would otherwise be lost (gmail `attachments` was the breakage seen
/// in the wild: an `array` payload was rejected after the union was
/// collapsed to `string`).
fn type_richness_rank(t: &str) -> u8 {
    match t {
        "array" => 5,
        "object" => 4,
        "string" => 3,
        "number" | "integer" => 2,
        "boolean" => 1,
        _ => 0,
    }
}

/// Maximum serialised payload size in bytes. Payloads exceeding this limit are
/// dropped to prevent oversized requests from consuming kernel resources.
pub const MAX_TOOL_PAYLOAD_BYTES: usize = 64 * 1024;

/// Infer an intent type string from a permission set.
///
/// Scans `ops` suffixes (after the `:`) for `x` (execute), `w` (write),
/// `r` (read) and returns the highest-privilege match.
pub fn infer_intent_type_from_permissions(permissions: &[String]) -> String {
    let mut has_read = false;
    let mut has_write = false;
    let mut has_execute = false;

    for permission in permissions {
        let ops = permission
            .split_once(':')
            .map(|(_, suffix)| suffix)
            .unwrap_or_default();
        if ops.contains('x') {
            has_execute = true;
        }
        if ops.contains('w') {
            has_write = true;
        }
        if ops.contains('r') {
            has_read = true;
        }
    }

    if has_execute {
        "execute".to_string()
    } else if has_write {
        "write".to_string()
    } else if has_read {
        "read".to_string()
    } else {
        "query".to_string()
    }
}

/// Ensure an input schema is a valid JSON Schema object.
///
/// If the schema is missing or not an object, returns a minimal
/// `{"type": "object", "properties": {}}` placeholder. Also walks the
/// schema recursively and replaces any null / non-object property value
/// with a permissive `{"type": "string"}` placeholder — some MCP servers
/// emit `{"properties": {"foo": null}}` which Ollama rejects with
/// `"None is not of type 'object'"`.
pub fn normalize_tool_input_schema(input_schema: Option<&Value>) -> Value {
    let mut schema = match input_schema.cloned() {
        Some(Value::Object(mut obj)) => {
            obj.entry("type".to_string())
                .or_insert_with(|| Value::String("object".to_string()));
            Value::Object(obj)
        }
        _ => json!({
            "type": "object",
            "properties": {}
        }),
    };
    sanitize_schema_node(&mut schema);
    ensure_object_has_properties(&mut schema);
    add_object_additional_properties_false(&mut schema);
    schema
}

/// Ollama's upstream tool-schema validator rejects object schemas that omit
/// `properties` with `"None is not of type 'object'"`. Walk the schema and
/// inject an empty `properties: {}` on every `type: "object"` node that lacks
/// one, matching the behaviour expected by jsonschema validators.
fn ensure_object_has_properties(schema: &mut Value) {
    match schema {
        Value::Object(obj) => {
            let is_object = obj.get("type").and_then(Value::as_str) == Some("object");
            if is_object {
                obj.entry("properties".to_string())
                    .or_insert_with(|| json!({}));
            }
            if let Some(Value::Object(properties)) = obj.get_mut("properties") {
                for value in properties.values_mut() {
                    ensure_object_has_properties(value);
                }
            }
            if let Some(items) = obj.get_mut("items") {
                ensure_object_has_properties(items);
            }
            for key in ["anyOf", "oneOf", "allOf"] {
                if let Some(Value::Array(variants)) = obj.get_mut(key) {
                    for value in variants {
                        ensure_object_has_properties(value);
                    }
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                ensure_object_has_properties(value);
            }
        }
        _ => {}
    }
}

/// Recursively replace null / non-object subschemas inside `properties`,
/// `items`, and combinator arms (`anyOf`/`oneOf`/`allOf`) with a permissive
/// `{"type": "string"}` placeholder. Object subschemas are recursed into.
fn sanitize_schema_node(node: &mut Value) {
    let Value::Object(obj) = node else {
        return;
    };

    // Collapse multi-type `"type": ["string","array"]` to a single scalar —
    // Ollama's tool schema validator only accepts scalar `type`, and even
    // adapters that *can* pass a union through have inconsistent behaviour
    // across providers. We pick the *richest* variant (array > object >
    // string > primitive) so that adapters and the downstream payload
    // validator stay aligned: an array payload survives, where collapsing
    // to "string" silently dropped it and broke gmail attachments in the
    // wild. The warning is de-duplicated per (variants, choice) pair so
    // it stays audible instead of becoming noise.
    if let Some(t) = obj.get("type").cloned() {
        if let Value::Array(variants) = t {
            let strs: Vec<String> = variants
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if strs.is_empty() {
                if warn_once(fingerprint(0, &["empty-type-array"])) {
                    warn!("Tool schema `type` array contained no string — dropping");
                }
                obj.remove("type");
            } else {
                let chosen = strs
                    .iter()
                    .max_by_key(|s| type_richness_rank(s.as_str()))
                    .cloned()
                    .expect("non-empty");
                let mut sorted = strs.clone();
                sorted.sort();
                let joined = sorted.join("|");
                let fp = fingerprint(1, &[joined.as_str(), chosen.as_str()]);
                if warn_once(fp) {
                    warn!(
                        variants = %joined,
                        chosen = %chosen,
                        "Tool schema multi-type collapsed (richest variant)"
                    );
                } else {
                    debug!(variants = %joined, chosen = %chosen, "Tool schema multi-type collapsed (cached)");
                }
                obj.insert("type".into(), Value::String(chosen));
            }
        } else if !matches!(t, Value::String(_)) {
            if warn_once(fingerprint(2, &["non-string-type"])) {
                warn!("Tool schema `type` is not a string or array — dropping");
            }
            obj.remove("type");
        }
    }

    if let Some(props) = obj.get_mut("properties") {
        if let Value::Object(map) = props {
            for value in map.values_mut() {
                if !matches!(value, Value::Object(_)) {
                    warn!(
                        original = %value,
                        "Tool schema property is not an object — replacing with permissive placeholder"
                    );
                    *value = json!({"type": "string"});
                } else {
                    sanitize_schema_node(value);
                }
            }
        } else {
            // `properties` itself wasn't an object — drop it.
            warn!("Tool schema `properties` is not an object — dropping");
            obj.remove("properties");
        }
    }

    if let Some(items) = obj.get_mut("items") {
        if !matches!(items, Value::Object(_) | Value::Array(_)) {
            *items = json!({"type": "string"});
        } else if let Value::Object(_) = items {
            sanitize_schema_node(items);
        } else if let Value::Array(arr) = items {
            for v in arr {
                if !matches!(v, Value::Object(_)) {
                    *v = json!({"type": "string"});
                } else {
                    sanitize_schema_node(v);
                }
            }
        }
    }

    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = obj.get_mut(key) {
            for v in variants {
                if !matches!(v, Value::Object(_)) {
                    *v = json!({"type": "string"});
                } else {
                    sanitize_schema_node(v);
                }
            }
        }
    }
}

/// OpenAI strict function calling rejects unconstrained object schemas. Keep
/// existing required/optional semantics, but close object shapes recursively so
/// extra fields cannot be smuggled into tool calls.
pub fn add_object_additional_properties_false(schema: &mut Value) {
    match schema {
        Value::Object(obj) => {
            let is_object = obj.get("type").and_then(Value::as_str) == Some("object")
                || obj.contains_key("properties");
            if is_object {
                obj.entry("additionalProperties".to_string())
                    .or_insert(Value::Bool(false));
            }

            if let Some(Value::Object(properties)) = obj.get_mut("properties") {
                for value in properties.values_mut() {
                    add_object_additional_properties_false(value);
                }
            }
            if let Some(items) = obj.get_mut("items") {
                add_object_additional_properties_false(items);
            }
            for key in ["anyOf", "oneOf", "allOf"] {
                if let Some(Value::Array(variants)) = obj.get_mut(key) {
                    for value in variants {
                        add_object_additional_properties_false(value);
                    }
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                add_object_additional_properties_false(value);
            }
        }
        _ => {}
    }
}

/// Return true when an object schema is safe to send with OpenAI `strict: true`
/// without changing tool semantics. OpenAI strict schemas require every object
/// property to be listed in `required`; many AgentOS tool manifests still use
/// optional fields with defaults, so those stay non-strict until their schemas
/// are explicitly migrated.
pub fn is_openai_strict_compatible_schema(schema: &Value) -> bool {
    match schema {
        Value::Object(obj) => {
            let is_object = obj.get("type").and_then(Value::as_str) == Some("object")
                || obj.contains_key("properties");
            if is_object {
                let property_names: Vec<&str> = obj
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|props| props.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                let required = obj
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<std::collections::HashSet<_>>()
                    })
                    .unwrap_or_default();
                if !property_names.iter().all(|name| required.contains(name)) {
                    return false;
                }
            }

            if let Some(Value::Object(properties)) = obj.get("properties") {
                if !properties.values().all(is_openai_strict_compatible_schema) {
                    return false;
                }
            }
            if let Some(items) = obj.get("items") {
                if !is_openai_strict_compatible_schema(items) {
                    return false;
                }
            }
            for key in ["anyOf", "oneOf", "allOf"] {
                if let Some(Value::Array(variants)) = obj.get(key) {
                    if !variants.iter().all(is_openai_strict_compatible_schema) {
                        return false;
                    }
                }
            }
            true
        }
        Value::Array(values) => values.iter().all(is_openai_strict_compatible_schema),
        _ => true,
    }
}

/// Check whether a serialised payload exceeds the size limit.
/// Returns `true` if the payload is within limits, `false` (with a warning) if oversized.
pub fn check_payload_size(tool_name: &str, payload: &Value) -> bool {
    let payload_bytes = serde_json::to_vec(payload).map(|b| b.len()).unwrap_or(0);
    if payload_bytes > MAX_TOOL_PAYLOAD_BYTES {
        warn!(
            tool_name,
            payload_bytes, "Skipping tool call with oversized payload"
        );
        return false;
    }
    true
}

/// Validate that a payload is a JSON object. Non-object values are
/// wrapped in `{"_raw": <value>}` with a warning.
pub fn validate_payload_object(tool_name: &str, provider: &str, value: Option<Value>) -> Value {
    match value {
        Some(Value::Object(obj)) => Value::Object(obj),
        Some(Value::Null) | None => json!({}),
        Some(other) => {
            warn!(
                tool_name,
                provider, "Tool call input was not an object; wrapping in _raw"
            );
            json!({"_raw": other})
        }
    }
}

/// Strip fenced JSON blocks from assistant text after tool calls were recovered from them,
/// so stored turns do not retain raw JSON that can confuse follow-up inference.
pub fn strip_tool_json_fences(text: &str, tool_call_count: usize) -> String {
    if tool_call_count == 0 {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;
    while pos < text.len() {
        if let Some(rel) = text[pos..].find("```") {
            let fence_open = pos + rel;
            result.push_str(&text[pos..fence_open]);
            let after_open = fence_open + 3;
            let line_end = text[after_open..]
                .find('\n')
                .map(|n| after_open + n + 1)
                .unwrap_or(text.len());
            let lang = text[after_open..line_end].trim().to_ascii_lowercase();
            let body_start = line_end;
            if let Some(close_rel) = text[body_start..].find("```") {
                let body_end = body_start + close_rel;
                let close_end = body_end + 3;
                if lang.is_empty() || lang == "json" {
                    let body = text[body_start..body_end].trim();
                    if serde_json::from_str::<Value>(body).is_ok() {
                        pos = close_end;
                        continue;
                    }
                }
                result.push_str(&text[fence_open..close_end]);
                pos = close_end;
            } else {
                result.push_str(&text[fence_open..]);
                pos = text.len();
            }
        } else {
            result.push_str(&text[pos..]);
            break;
        }
    }
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_intent_type_from_permissions() {
        assert_eq!(
            infer_intent_type_from_permissions(&["fs.user_data:r".to_string()]),
            "read"
        );
        assert_eq!(
            infer_intent_type_from_permissions(&["fs.user_data:rw".to_string()]),
            "write"
        );
        assert_eq!(
            infer_intent_type_from_permissions(&["shell:x".to_string()]),
            "execute"
        );
        assert_eq!(
            infer_intent_type_from_permissions(&["memory:".to_string()]),
            "query"
        );
        assert_eq!(infer_intent_type_from_permissions(&[]), "query");
    }

    #[test]
    fn test_normalize_tool_input_schema_adds_type() {
        let schema = json!({"properties": {"path": {"type": "string"}}});
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["properties"]["path"]["type"], "string");
    }

    #[test]
    fn test_normalize_tool_input_schema_none() {
        let normalized = normalize_tool_input_schema(None);
        assert_eq!(normalized["type"], "object");
    }

    #[test]
    fn test_normalize_tool_input_schema_inserts_empty_properties() {
        // Tools like `datetime`, `memory-stats`, `context-memory-read` ship
        // `{"type": "object"}` with no `properties` key. Ollama's upstream
        // tool schema validator rejects these with `None is not of type 'object'`.
        // Normalizer must inject an empty `properties: {}` object.
        let schema = json!({"type": "object", "description": "No input required"});
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert_eq!(normalized["type"], "object");
        assert!(normalized["properties"].is_object());
        assert_eq!(normalized["properties"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn test_normalize_tool_input_schema_inserts_nested_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "nested": {"type": "object"},
            },
        });
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert!(normalized["properties"]["nested"]["properties"].is_object());
    }

    #[test]
    fn test_normalize_tool_input_schema_replaces_null_property() {
        // Some MCP servers emit `{"properties": {"foo": null}}` which Ollama
        // rejects with `"None is not of type 'object'"`. Property values must
        // become object schemas.
        let schema = json!({
            "type": "object",
            "properties": {
                "good": {"type": "string"},
                "bad": null,
            },
        });
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert!(normalized["properties"]["bad"].is_object());
        assert_eq!(normalized["properties"]["bad"]["type"], "string");
        assert_eq!(normalized["properties"]["good"]["type"], "string");
    }

    #[test]
    fn test_normalize_tool_input_schema_replaces_nested_null_property() {
        let schema = json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "properties": {"inner": null},
                },
            },
        });
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert!(normalized["properties"]["outer"]["properties"]["inner"].is_object());
        assert_eq!(
            normalized["properties"]["outer"]["properties"]["inner"]["type"],
            "string"
        );
    }

    #[test]
    fn test_normalize_tool_input_schema_drops_non_object_properties_field() {
        // `properties: "garbage"` (non-object) is dropped by the sanitizer,
        // then `ensure_object_has_properties` re-injects an empty `{}` so
        // Ollama's validator accepts the resulting object schema.
        let schema = json!({"type": "object", "properties": "garbage"});
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert!(normalized["properties"].is_object());
        assert_eq!(normalized["properties"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn test_normalize_tool_input_schema_collapses_multi_type_to_richest() {
        // Multi-type unions like `"type": ["string","array"]` (memory-write
        // tags, gmail-send attachments) must collapse to the *richest*
        // variant — array beats string — so an array payload survives
        // schema validation downstream. Picking "string" silently dropped
        // the array branch and broke gmail attachments in the wild.
        let schema = json!({
            "type": "object",
            "properties": {
                "tags": {
                    "type": ["string", "array"],
                    "items": {"type": "string"},
                },
            },
        });
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert_eq!(normalized["properties"]["tags"]["type"], "array");
        // `items` survives the collapse so the array shape stays usable.
        assert_eq!(normalized["properties"]["tags"]["items"]["type"], "string");
    }

    #[test]
    fn test_normalize_tool_input_schema_collapse_prefers_object_over_string() {
        let schema = json!({
            "type": "object",
            "properties": {
                "payload": {"type": ["string", "object"], "properties": {"k": {"type": "string"}}},
            },
        });
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert_eq!(normalized["properties"]["payload"]["type"], "object");
    }

    #[test]
    fn test_warn_once_cache_drains_at_cap() {
        // Stream more than `WARN_ONCE_CACHE_CAP` distinct fingerprints
        // and confirm the cache stays bounded. Drain semantics: once
        // we hit the cap we clear and start fresh.
        let cache = warn_once_cache();
        // Fresh fingerprints unique to this test (use high seed bits
        // to avoid colliding with anything emitted by other tests
        // running in parallel).
        for i in 0..(WARN_ONCE_CACHE_CAP + 100) {
            warn_once(fingerprint(99, &[&format!("cap-test-{i}")]));
        }
        let len = cache.lock().unwrap().len();
        assert!(
            len <= WARN_ONCE_CACHE_CAP,
            "cache len {} exceeded cap {}",
            len,
            WARN_ONCE_CACHE_CAP
        );
    }

    #[test]
    fn test_normalize_tool_input_schema_collapse_falls_back_to_only_scalar() {
        // When the union has only primitives, the highest-rank scalar
        // wins (string > number > boolean).
        let schema = json!({
            "type": "object",
            "properties": {"x": {"type": ["boolean", "number", "string"]}},
        });
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert_eq!(normalized["properties"]["x"]["type"], "string");
    }

    #[test]
    fn test_normalize_tool_input_schema_drops_multi_type_with_no_strings() {
        let schema = json!({
            "type": "object",
            "properties": {"x": {"type": [1, 2]}},
        });
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert!(normalized["properties"]["x"].get("type").is_none());
    }

    #[test]
    fn test_normalize_tool_input_schema_sanitizes_anyof_null() {
        let schema = json!({
            "type": "object",
            "properties": {
                "x": {"anyOf": [{"type": "string"}, null]},
            },
        });
        let normalized = normalize_tool_input_schema(Some(&schema));
        let variants = &normalized["properties"]["x"]["anyOf"];
        assert!(variants[0].is_object());
        assert!(variants[1].is_object());
    }

    #[test]
    fn test_check_payload_size_within_limit() {
        let payload = json!({"key": "value"});
        assert!(check_payload_size("test-tool", &payload));
    }

    #[test]
    fn test_check_payload_size_oversized() {
        let big = "x".repeat(MAX_TOOL_PAYLOAD_BYTES + 1);
        let payload = json!({"data": big});
        assert!(!check_payload_size("test-tool", &payload));
    }

    #[test]
    fn test_validate_payload_object_with_object() {
        let val = Some(json!({"key": "value"}));
        let result = validate_payload_object("tool", "test", val);
        assert_eq!(result["key"], "value");
    }

    #[test]
    fn test_validate_payload_object_with_string() {
        let val = Some(json!("not an object"));
        let result = validate_payload_object("tool", "test", val);
        assert_eq!(result["_raw"], "not an object");
    }

    #[test]
    fn test_validate_payload_object_none() {
        let result = validate_payload_object("tool", "test", None);
        assert_eq!(result, json!({}));
    }
}
