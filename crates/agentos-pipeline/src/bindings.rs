//! Resolving `{{binding}}` references inside a tool step's JSON payload.
//!
//! The older `render_template_for_json` serializes the payload, substitutes
//! into the resulting *text*, and re-parses. That works only because every
//! value is escaped on the way in, and it can only ever produce strings: a
//! payload field that should be a number, an object or an array cannot be bound
//! to one. Executable procedures need both — `speak` returns an object and the
//! next step wants `{{clip.path}}` out of it, and a declared input may be any
//! JSON type.
//!
//! So this renderer walks the `Value` instead. Nothing is ever serialized and
//! re-parsed, which means a bound value containing quotes, backslashes or
//! newlines cannot corrupt the payload's structure — not because it is escaped
//! correctly, but because there is no text to escape into.
//!
//! Grammar: `{{ root[.field…] }}`. The root is `[A-Za-z_][A-Za-z0-9_]*`; each
//! later segment is `[A-Za-z0-9_]+`, so an array index (`{{rows.0.id}}`) is a
//! path segment like any other.
//!
//! [`is_valid_path`] is exported so the authoring validator in
//! `procedure-create` can accept exactly this grammar and nothing else. The two
//! MUST agree: a path the validator accepts and this regex does not is left in
//! the payload as literal `{{…}}` text and handed to a live tool.

use regex::Regex;
use serde_json::Value;
use std::collections::BTreeMap;

/// Values a step's payload may reference, keyed by root name.
pub type Bindings = BTreeMap<String, Value>;

/// `{{ path }}`, dotted, with optional surrounding whitespace.
///
/// Whitespace is tolerated because `procedure-create` trims it when validating,
/// so `{{ text }}` passes authoring and must therefore resolve here too — the
/// two have to accept exactly the same set of strings.
fn binding_regex() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(PATTERN).expect("static regex is valid"));
    &RE
}

/// The binding body, without the surrounding braces. Shared with [`is_valid_path`]
/// so one edit changes both.
const PATH_BODY: &str = r"[a-zA-Z_][a-zA-Z0-9_]*(?:\.[a-zA-Z0-9_]+)*";
const PATTERN: &str = r"\{\{\s*([a-zA-Z_][a-zA-Z0-9_]*(?:\.[a-zA-Z0-9_]+)*)\s*\}\}";

/// True if `path` is a binding this module can actually resolve.
///
/// Exported for `procedure-create`: the authoring validator must accept exactly
/// what the renderer accepts. When it was laxer, `{{rows.0.id}}` and
/// `{{clip.path-x}}` passed authoring, matched no binding at render time, and
/// reached a real gated tool as the literal string `{{rows.0.id}}`.
pub fn is_valid_path(path: &str) -> bool {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(&format!("^{PATH_BODY}$")).expect("static regex is valid")
    });
    RE.is_match(path)
}

/// The root segment of a binding path.
pub fn path_root(path: &str) -> &str {
    path.split('.').next().unwrap_or(path)
}

/// The one root the caller's declared inputs bind under.
///
/// Reserved: a step `output_var` of this name would overwrite the caller's
/// parameters mid-run, so every later `{{inputs.x}}` would read a field of that
/// step's output instead. Where the step fetches something external, that is a
/// parameter-injection primitive in an unattended, agent-permissioned run.
pub const INPUTS_ROOT: &str = "inputs";

/// Follow a dotted path into the bound values.
///
/// Objects are indexed by key and arrays by decimal index, so `{{rows.0.id}}`
/// works without special-casing. Returns `None` for a missing root, a missing
/// field, or an index into a scalar.
pub fn resolve(bindings: &Bindings, path: &str) -> Option<Value> {
    let mut segments = path.split('.');
    let mut current = bindings.get(segments.next()?)?;
    for segment in segments {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current.clone())
}

/// What a string renders to when it is exactly one binding.
enum Whole<'a> {
    /// The string is `{{path}}` and nothing else.
    Binding(&'a str),
    /// Anything else — zero bindings, or one among other text.
    Mixed,
}

fn whole_binding(text: &str) -> Whole<'_> {
    match binding_regex().find(text) {
        Some(m) if m.start() == 0 && m.end() == text.len() => {
            // `captures` re-runs the match; cheap, and avoids re-deriving the
            // group bounds by hand.
            match binding_regex().captures(text).and_then(|c| c.get(1)) {
                Some(g) => Whole::Binding(&text[g.start()..g.end()]),
                None => Whole::Mixed,
            }
        }
        _ => Whole::Mixed,
    }
}

/// Marker left in place of a binding that resolved to nothing.
///
/// Deliberately visible rather than an empty string: a step that received a
/// silently-blank field would fail somewhere far from the cause, and a payload
/// that still looks well-formed is the worst kind of bug to chase.
fn unresolved_marker(path: &str) -> String {
    format!("{{{{UNRESOLVED:{path}}}}}")
}

/// Render every binding in `template`, and report any that did not resolve.
///
/// The caller decides what an unresolved binding means. For a procedure it is
/// fatal — a step must not run with `{{UNRESOLVED:inputs.voice}}` sitting in
/// its `audio_path` or `command` — so `execute_step` fails the step instead of
/// firing it. The marker is the right value for a log, not for a payload.
pub fn render_checked(template: &Value, bindings: &Bindings) -> (Value, Vec<String>) {
    let mut unresolved = Vec::new();
    let rendered = render_inner(template, bindings, &mut unresolved);
    (rendered, unresolved)
}

/// Render every binding in `template` against `bindings`.
///
/// A string that is *exactly* one binding takes that value's own type — so
/// `"{{clip}}"` against an object yields the object, not its text. A binding
/// among other text is stringified and interpolated, because the surrounding
/// text only makes sense as a string.
pub fn render(template: &Value, bindings: &Bindings) -> Value {
    render_checked(template, bindings).0
}

fn render_inner(template: &Value, bindings: &Bindings, unresolved: &mut Vec<String>) -> Value {
    match template {
        Value::String(text) => match whole_binding(text) {
            Whole::Binding(path) => resolve(bindings, path).unwrap_or_else(|| {
                tracing::warn!(binding = path, "Unresolved step binding");
                unresolved.push(path.to_string());
                Value::String(unresolved_marker(path))
            }),
            Whole::Mixed => Value::String(
                binding_regex()
                    .replace_all(text, |caps: &regex::Captures| {
                        let path = &caps[1];
                        match resolve(bindings, path) {
                            // A string interpolates as its contents, not as a
                            // quoted JSON string.
                            Some(Value::String(s)) => s,
                            Some(other) => other.to_string(),
                            None => {
                                tracing::warn!(binding = path, "Unresolved step binding");
                                unresolved.push(path.to_string());
                                unresolved_marker(path)
                            }
                        }
                    })
                    .into_owned(),
            ),
        },
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| render_inner(v, bindings, unresolved))
                .collect(),
        ),
        // Object KEYS are left alone on purpose. A tool's payload keys are its
        // API; letting a recipe compute one would mean the approved template no
        // longer tells you which fields a call sets.
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), render_inner(v, bindings, unresolved)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bindings() -> Bindings {
        BTreeMap::from([
            (
                "clip".to_string(),
                json!({ "path": "speech/a.wav", "bytes": 42, "meta": { "voice": "af_heart" } }),
            ),
            ("inputs".to_string(), json!({ "text": "hello", "n": 3 })),
            ("rows".to_string(), json!([{ "id": "r0" }, { "id": "r1" }])),
        ])
    }

    #[test]
    fn a_whole_binding_keeps_the_values_type() {
        let out = render(
            &json!({ "s": "{{clip.path}}", "n": "{{clip.bytes}}", "o": "{{clip.meta}}" }),
            &bindings(),
        );
        assert_eq!(out["s"], json!("speech/a.wav"));
        // The payload field is a NUMBER, not the string "42" — this is the
        // whole reason for walking the value instead of the text.
        assert_eq!(out["n"], json!(42));
        assert_eq!(out["o"], json!({ "voice": "af_heart" }));
    }

    #[test]
    fn a_binding_among_text_interpolates_as_a_string() {
        let out = render(
            &json!({ "m": "said {{inputs.text}} ({{inputs.n}}x)" }),
            &bindings(),
        );
        assert_eq!(out["m"], json!("said hello (3x)"));
    }

    /// The reason this renderer exists rather than splicing into serialized
    /// text: a value full of JSON metacharacters cannot reshape the payload,
    /// because there is no text for it to escape out of.
    #[test]
    fn a_hostile_value_cannot_reshape_the_payload() {
        let hostile = r#"", "injected": "yes"#;
        let bound = Bindings::from([("inputs".to_string(), json!({ "text": hostile }))]);
        let out = render(&json!({ "text": "{{inputs.text}}", "keep": 1 }), &bound);
        assert_eq!(out["text"], json!(hostile));
        assert_eq!(out["keep"], json!(1));
        assert!(out.get("injected").is_none(), "{out}");
        assert_eq!(out.as_object().unwrap().len(), 2);
    }

    #[test]
    fn arrays_are_indexed_by_number() {
        assert_eq!(resolve(&bindings(), "rows.1.id"), Some(json!("r1")));
        assert_eq!(resolve(&bindings(), "rows.9.id"), None);
        assert_eq!(resolve(&bindings(), "rows.x"), None);
    }

    #[test]
    fn indexing_into_a_scalar_resolves_to_nothing() {
        assert_eq!(resolve(&bindings(), "clip.bytes.nope"), None);
        assert_eq!(resolve(&bindings(), "missing"), None);
        assert_eq!(resolve(&bindings(), "clip.missing"), None);
    }

    /// An unresolved binding must stay visible. A silently-empty field fails
    /// later, somewhere unrelated, with a payload that still looks well-formed.
    #[test]
    fn an_unresolved_binding_is_marked_not_blanked() {
        let out = render(
            &json!({ "a": "{{nope}}", "b": "x {{nope.y}} z" }),
            &bindings(),
        );
        assert_eq!(out["a"], json!("{{UNRESOLVED:nope}}"));
        assert_eq!(out["b"], json!("x {{UNRESOLVED:nope.y}} z"));
    }

    /// The caller has to be able to tell "resolved to nothing" from "resolved
    /// to a marker-shaped string", because a procedure step must not fire with
    /// `{{UNRESOLVED:…}}` sitting in its `audio_path` or `command`.
    #[test]
    fn render_checked_reports_every_unresolved_binding() {
        let (value, unresolved) = render_checked(
            &json!({ "a": "{{nope}}", "b": "x {{also.missing}} y", "c": "{{clip.path}}" }),
            &bindings(),
        );
        assert_eq!(unresolved, vec!["nope", "also.missing"]);
        assert_eq!(
            value["c"],
            json!("speech/a.wav"),
            "resolved ones still render"
        );

        let (_, none_missing) = render_checked(&json!({ "c": "{{clip.path}}" }), &bindings());
        assert!(none_missing.is_empty());
    }

    #[test]
    fn whitespace_inside_the_braces_is_tolerated() {
        // procedure-create trims when validating, so authoring accepts this and
        // rendering has to as well or a validated recipe fails at run time.
        let out = render(&json!({ "a": "{{ clip.path }}" }), &bindings());
        assert_eq!(out["a"], json!("speech/a.wav"));
    }

    #[test]
    fn object_keys_are_never_rendered() {
        let out = render(&json!({ "{{inputs.text}}": "v" }), &bindings());
        assert_eq!(out, json!({ "{{inputs.text}}": "v" }));
    }

    #[test]
    fn non_string_scalars_pass_through() {
        let out = render(
            &json!({ "n": 1, "b": true, "z": null, "a": [1, "{{inputs.n}}"] }),
            &bindings(),
        );
        assert_eq!(out, json!({ "n": 1, "b": true, "z": null, "a": [1, 3] }));
    }

    /// `{{a{{b}}c}}` renders its one legal inner binding and leaves the outer
    /// braces as text.
    ///
    /// The authoring validator is STRICTER here: its hand-rolled scanner reads
    /// from the first `{{` to the first `}}`, extracts `a{{b`, and rejects the
    /// recipe. That asymmetry is fail-closed — the validator refuses what this
    /// would resolve — so no unvalidated binding can reach a tool. It is not
    /// the two sides agreeing, and a future reader should not assume it is.
    #[test]
    fn nested_braces_resolve_only_the_inner_binding() {
        let bound = Bindings::from([("b".to_string(), json!("B"))]);
        let out = render(&json!({ "x": "{{a{{b}}c}}" }), &bound);
        assert_eq!(out["x"], json!("{{aBc}}"));
    }
}
