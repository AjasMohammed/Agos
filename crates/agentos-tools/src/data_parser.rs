use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;
use std::sync::LazyLock;

static MARKDOWN_LINK_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid regex"));

pub struct DataParser;

impl DataParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DataParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for DataParser {
    fn name(&self) -> &str {
        "data-parser"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // No permissions required — parses in-memory data only, no I/O.
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        const MAX_DATA_BYTES: usize = 4 * 1024 * 1024;
        const MAX_ROWS: usize = 50_000;

        let data = payload
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("data-parser requires 'data' field (string)".into())
            })?;

        if data.len() > MAX_DATA_BYTES {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!(
                    "Input too large: {} bytes (limit {} bytes)",
                    data.len(),
                    MAX_DATA_BYTES
                ),
            });
        }

        let format = payload
            .get("format")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "data-parser requires 'format' field \
                     (json|csv|tsv|toml|yaml|xml|jsonl|markdown)"
                        .into(),
                )
            })?;

        let infer_types = payload
            .get("infer_types")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let query = payload.get("query").and_then(|v| v.as_str());
        let offset = payload.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let output_format = payload.get("output_format").and_then(|v| v.as_str());

        tracing::debug!(format, input_bytes = data.len(), "data-parser: starting");

        // 1. Parse
        let mut parsed = parse_format(data, format, infer_types, MAX_ROWS)?;
        tracing::debug!(format, "data-parser: parse complete");

        // 2. Path query
        if let Some(path) = query {
            parsed = apply_query(parsed, path)?;
        }

        // 3. Paginate (only when caller explicitly requests it)
        let should_paginate = offset > 0 || limit.is_some();
        let (parsed, pagination) = if should_paginate {
            paginate(parsed, offset, limit)
        } else {
            (parsed, None)
        };

        // 4. Re-serialize to a different format if requested
        if let Some(out_fmt) = output_format {
            let output = serialize_to(&parsed, out_fmt)?;
            let mut result = serde_json::json!({
                "format": format,
                "output_format": out_fmt,
                "output": output,
            });
            if let Some(p) = pagination {
                result["total"] = serde_json::json!(p.total);
                result["offset"] = serde_json::json!(p.offset);
                result["limit"] = serde_json::json!(p.limit);
                result["has_more"] = serde_json::json!(p.has_more);
            }
            return Ok(result);
        }

        let mut result = serde_json::json!({
            "format": format,
            "parsed": parsed,
        });

        if let Some(p) = pagination {
            result["total"] = serde_json::json!(p.total);
            result["offset"] = serde_json::json!(p.offset);
            result["limit"] = serde_json::json!(p.limit);
            result["has_more"] = serde_json::json!(p.has_more);
        }

        Ok(result)
    }
}

// ── Pagination metadata ──────────────────────────────────────────────────────

struct PaginationMeta {
    total: usize,
    offset: usize,
    limit: usize,
    has_more: bool,
}

// ── Format dispatch ──────────────────────────────────────────────────────────

fn parse_format(
    data: &str,
    format: &str,
    infer_types: bool,
    max_rows: usize,
) -> Result<serde_json::Value, AgentOSError> {
    match format.to_lowercase().as_str() {
        "json" => parse_json(data),
        "csv" => parse_delimited(data, b',', infer_types, max_rows),
        "tsv" => parse_delimited(data, b'\t', infer_types, max_rows),
        "toml" => parse_toml(data),
        "yaml" | "yml" => parse_yaml(data),
        "xml" => parse_xml(data),
        "jsonl" | "ndjson" => parse_jsonl(data, max_rows),
        "markdown" | "md" => parse_markdown(data),
        other => Err(AgentOSError::SchemaValidation(format!(
            "Unsupported format: '{}'. Supported: json, csv, tsv, toml, yaml, xml, jsonl, markdown",
            other
        ))),
    }
}

// ── Individual parsers ───────────────────────────────────────────────────────

fn parse_json(data: &str) -> Result<serde_json::Value, AgentOSError> {
    serde_json::from_str(data).map_err(|e| AgentOSError::ToolExecutionFailed {
        tool_name: "data-parser".into(),
        reason: format!("Invalid JSON: {}", e),
    })
}

fn parse_delimited(
    data: &str,
    delimiter: u8,
    infer_types: bool,
    max_rows: usize,
) -> Result<serde_json::Value, AgentOSError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .delimiter(delimiter)
        .from_reader(data.as_bytes());

    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "data-parser".into(),
            reason: format!("Invalid headers: {}", e),
        })?
        .iter()
        .map(|h| h.to_string())
        .collect();

    let mut rows = Vec::new();
    for record in reader.records() {
        if rows.len() >= max_rows {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!("Input exceeds maximum row count of {}", max_rows),
            });
        }
        let record = record.map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "data-parser".into(),
            reason: format!("Invalid row: {}", e),
        })?;
        let mut row = serde_json::Map::new();
        for (i, field) in record.iter().enumerate() {
            if let Some(header) = headers.get(i) {
                let value = if infer_types {
                    coerce_value(field)
                } else {
                    serde_json::Value::String(field.to_string())
                };
                row.insert(header.clone(), value);
            }
        }
        rows.push(serde_json::Value::Object(row));
    }

    let row_count = rows.len();
    Ok(serde_json::json!({
        "headers": headers,
        "rows": rows,
        "row_count": row_count,
    }))
}

fn parse_toml(data: &str) -> Result<serde_json::Value, AgentOSError> {
    let value: toml::Value =
        toml::from_str(data).map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "data-parser".into(),
            reason: format!("Invalid TOML: {}", e),
        })?;
    serde_json::to_value(value).map_err(|e| AgentOSError::ToolExecutionFailed {
        tool_name: "data-parser".into(),
        reason: format!("TOML to JSON conversion failed: {}", e),
    })
}

fn parse_yaml(data: &str) -> Result<serde_json::Value, AgentOSError> {
    let value: serde_json::Value =
        serde_yaml::from_str(data).map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "data-parser".into(),
            reason: format!("Invalid YAML: {}", e),
        })?;
    // Guard against YAML alias/anchor bombs: reject if expanded output is >10x input size.
    let serialized_size = serde_json::to_string(&value).map(|s| s.len()).unwrap_or(0);
    if serialized_size > data.len() * 10 {
        return Err(AgentOSError::ToolExecutionFailed {
            tool_name: "data-parser".into(),
            reason: format!(
                "YAML expansion too large ({} bytes from {} byte input, possible YAML bomb)",
                serialized_size,
                data.len()
            ),
        });
    }
    Ok(value)
}

type XmlFrame = (
    String,
    serde_json::Map<String, serde_json::Value>,
    Vec<serde_json::Value>,
    String,
);

fn parse_xml(data: &str) -> Result<serde_json::Value, AgentOSError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    const MAX_XML_DEPTH: usize = 512;

    let mut reader = Reader::from_str(data);
    reader.config_mut().trim_text(true);

    // Each stack frame: (tag, attrs, children, text_accumulator)
    let mut stack: Vec<XmlFrame> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if stack.len() >= MAX_XML_DEPTH {
                    return Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("XML nesting exceeds maximum depth of {}", MAX_XML_DEPTH),
                    });
                }
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let attrs = collect_xml_attrs(&e);
                stack.push((tag, attrs, Vec::new(), String::new()));
            }
            Ok(Event::Empty(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let attrs = collect_xml_attrs(&e);
                let node = xml_node_to_value(tag, attrs, Vec::new(), String::new());
                if let Some(parent) = stack.last_mut() {
                    parent.2.push(node);
                } else {
                    return Ok(node);
                }
            }
            Ok(Event::End(_)) => {
                if let Some((tag, attrs, children, text)) = stack.pop() {
                    let node = xml_node_to_value(tag, attrs, children, text);
                    if let Some(parent) = stack.last_mut() {
                        parent.2.push(node);
                    } else {
                        return Ok(node);
                    }
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                if let Some(top) = stack.last_mut() {
                    top.3.push_str(&text);
                }
            }
            Ok(Event::CData(e)) => {
                let text = String::from_utf8_lossy(e.as_ref()).to_string();
                if let Some(top) = stack.last_mut() {
                    top.3.push_str(&text);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "data-parser".into(),
                    reason: format!("Invalid XML: {}", e),
                });
            }
            _ => {}
        }
        buf.clear();
    }

    Err(AgentOSError::ToolExecutionFailed {
        tool_name: "data-parser".into(),
        reason: "XML document has no root element".into(),
    })
}

fn collect_xml_attrs(
    e: &quick_xml::events::BytesStart,
) -> serde_json::Map<String, serde_json::Value> {
    let mut attrs = serde_json::Map::new();
    for attr in e.attributes().flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
        let val = String::from_utf8_lossy(attr.value.as_ref()).to_string();
        attrs.insert(key, serde_json::Value::String(val));
    }
    attrs
}

fn xml_node_to_value(
    tag: String,
    attrs: serde_json::Map<String, serde_json::Value>,
    children: Vec<serde_json::Value>,
    text: String,
) -> serde_json::Value {
    // Collapse repeated child tags into arrays
    let mut child_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for child in children {
        if let serde_json::Value::Object(ref obj) = child {
            if let Some(serde_json::Value::String(child_tag)) = obj.get("tag") {
                let key = child_tag.clone();
                match child_map.get_mut(&key) {
                    Some(serde_json::Value::Array(arr)) => arr.push(child),
                    Some(existing) => {
                        let prev = existing.clone();
                        *existing = serde_json::json!([prev, child]);
                    }
                    None => {
                        child_map.insert(key, child);
                    }
                }
            }
        }
    }

    let mut node = serde_json::Map::new();
    node.insert("tag".into(), serde_json::Value::String(tag));
    if !attrs.is_empty() {
        node.insert("attrs".into(), serde_json::Value::Object(attrs));
    }
    let trimmed = text.trim().to_string();
    if !trimmed.is_empty() {
        node.insert("text".into(), serde_json::Value::String(trimmed));
    }
    if !child_map.is_empty() {
        node.insert("children".into(), serde_json::Value::Object(child_map));
    }
    serde_json::Value::Object(node)
}

fn parse_jsonl(data: &str, max_rows: usize) -> Result<serde_json::Value, AgentOSError> {
    let mut records = Vec::new();
    for (i, line) in data.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if records.len() >= max_rows {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!("JSONL input exceeds maximum record count of {}", max_rows),
            });
        }
        let val: serde_json::Value =
            serde_json::from_str(line).map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!("Invalid JSON on line {}: {}", i + 1, e),
            })?;
        records.push(val);
    }
    let count = records.len();
    Ok(serde_json::json!({
        "records": records,
        "count": count,
    }))
}

fn parse_markdown(data: &str) -> Result<serde_json::Value, AgentOSError> {
    let lines: Vec<&str> = data.lines().collect();
    let total = lines.len();
    let mut i = 0;

    // Extract YAML frontmatter between opening and closing ---
    let mut frontmatter: Option<serde_json::Value> = None;
    if lines.first().map(|l| l.trim()) == Some("---") {
        let mut fm_lines: Vec<&str> = Vec::new();
        i = 1;
        while i < total && lines[i].trim() != "---" {
            fm_lines.push(lines[i]);
            i += 1;
        }
        if i < total {
            i += 1; // skip closing ---
            if let Ok(val) = serde_yaml::from_str::<serde_json::Value>(&fm_lines.join("\n")) {
                frontmatter = Some(val);
            }
        }
    }

    let mut headings: Vec<serde_json::Value> = Vec::new();
    let mut code_blocks: Vec<serde_json::Value> = Vec::new();
    let mut links: Vec<serde_json::Value> = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf: Vec<&str> = Vec::new();

    while i < total {
        let line = lines[i];

        // Code fence
        if line.starts_with("```") {
            if in_code {
                code_blocks.push(serde_json::json!({
                    "language": code_lang,
                    "code": code_buf.join("\n"),
                }));
                code_buf.clear();
                code_lang = String::new();
                in_code = false;
            } else {
                code_lang = line.trim_start_matches('`').trim().to_string();
                in_code = true;
            }
            i += 1;
            continue;
        }

        if in_code {
            code_buf.push(line);
            i += 1;
            continue;
        }

        // ATX headings
        if line.starts_with('#') {
            let level = line.chars().take_while(|c| *c == '#').count();
            let text = line[level..].trim().to_string();
            headings.push(serde_json::json!({ "level": level, "text": text }));
        }

        // Inline links: [text](url)
        for cap in MARKDOWN_LINK_RE.captures_iter(line) {
            links.push(serde_json::json!({
                "text": &cap[1],
                "url": &cap[2],
            }));
        }

        i += 1;
    }

    let word_count = data.split_whitespace().count();
    let char_count = data.chars().count();

    let mut result = serde_json::Map::new();
    if let Some(fm) = frontmatter {
        result.insert("frontmatter".into(), fm);
    }
    result.insert("headings".into(), serde_json::Value::Array(headings));
    result.insert("code_blocks".into(), serde_json::Value::Array(code_blocks));
    result.insert("links".into(), serde_json::Value::Array(links));
    result.insert("word_count".into(), serde_json::json!(word_count));
    result.insert("char_count".into(), serde_json::json!(char_count));
    Ok(serde_json::Value::Object(result))
}

// ── Type coercion for CSV/TSV ────────────────────────────────────────────────

fn coerce_value(s: &str) -> serde_json::Value {
    if s.eq_ignore_ascii_case("true") {
        return serde_json::Value::Bool(true);
    }
    if s.eq_ignore_ascii_case("false") {
        return serde_json::Value::Bool(false);
    }
    if s.is_empty() {
        return serde_json::Value::Null;
    }
    if let Ok(n) = s.parse::<i64>() {
        return serde_json::json!(n);
    }
    if let Ok(f) = s.parse::<f64>() {
        return serde_json::json!(f);
    }
    serde_json::Value::String(s.to_string())
}

// ── Path query ───────────────────────────────────────────────────────────────

/// Apply a dot-path query to a parsed value.
/// Syntax: `.key`, `.key.nested`, `.[0]`, `.key[0].field`
fn apply_query(value: serde_json::Value, path: &str) -> Result<serde_json::Value, AgentOSError> {
    #[derive(Debug)]
    enum Token {
        Key(String),
        Index(usize),
    }

    fn tokenize(path: &str) -> Result<Vec<Token>, AgentOSError> {
        let path = path.trim_start_matches('.');
        let mut tokens = Vec::new();
        let mut chars = path.chars().peekable();
        let mut buf = String::new();

        while let Some(c) = chars.next() {
            match c {
                '.' => {
                    if !buf.is_empty() {
                        tokens.push(Token::Key(buf.clone()));
                        buf.clear();
                    }
                }
                '[' => {
                    if !buf.is_empty() {
                        tokens.push(Token::Key(buf.clone()));
                        buf.clear();
                    }
                    let mut idx_str = String::new();
                    let mut found_close = false;
                    for ic in chars.by_ref() {
                        if ic == ']' {
                            found_close = true;
                            break;
                        }
                        idx_str.push(ic);
                    }
                    if !found_close {
                        return Err(AgentOSError::SchemaValidation(format!(
                            "Unclosed bracket in query path: '[{}'",
                            idx_str
                        )));
                    }
                    let idx = idx_str.parse::<usize>().map_err(|_| {
                        AgentOSError::SchemaValidation(format!(
                            "Invalid array index in query path: '[{}]'",
                            idx_str
                        ))
                    })?;
                    tokens.push(Token::Index(idx));
                }
                other => buf.push(other),
            }
        }
        if !buf.is_empty() {
            tokens.push(Token::Key(buf));
        }
        Ok(tokens)
    }

    let tokens = tokenize(path)?;
    let mut current = value;
    for token in &tokens {
        match token {
            Token::Key(key) => {
                current =
                    current
                        .get(key)
                        .cloned()
                        .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                            tool_name: "data-parser".into(),
                            reason: format!("Query path key '{}' not found", key),
                        })?;
            }
            Token::Index(idx) => {
                current =
                    current
                        .get(idx)
                        .cloned()
                        .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                            tool_name: "data-parser".into(),
                            reason: format!("Query path index [{}] out of range", idx),
                        })?;
            }
        }
    }
    Ok(current)
}

// ── Pagination ───────────────────────────────────────────────────────────────

/// Paginate a parsed value. Handles:
/// - Top-level arrays
/// - CSV/TSV: objects with `rows` + `headers`
/// - JSONL: objects with `records`
fn paginate(
    value: serde_json::Value,
    offset: usize,
    limit: Option<usize>,
) -> (serde_json::Value, Option<PaginationMeta>) {
    let take = limit.unwrap_or(usize::MAX);

    match value {
        serde_json::Value::Array(arr) => {
            let total = arr.len();
            let sliced: Vec<serde_json::Value> = arr.into_iter().skip(offset).take(take).collect();
            let returned = sliced.len();
            let has_more = offset + returned < total;
            (
                serde_json::Value::Array(sliced),
                Some(PaginationMeta {
                    total,
                    offset,
                    limit: limit.unwrap_or(returned),
                    has_more,
                }),
            )
        }
        serde_json::Value::Object(ref obj)
            if obj.contains_key("rows") && obj.contains_key("headers") =>
        {
            if let Some(serde_json::Value::Array(rows)) = obj.get("rows") {
                let total = rows.len();
                let sliced: Vec<serde_json::Value> =
                    rows.iter().skip(offset).take(take).cloned().collect();
                let returned = sliced.len();
                let has_more = offset + returned < total;
                let mut new_obj = obj.clone();
                new_obj.insert("rows".into(), serde_json::Value::Array(sliced));
                new_obj.insert("row_count".into(), serde_json::json!(returned));
                return (
                    serde_json::Value::Object(new_obj),
                    Some(PaginationMeta {
                        total,
                        offset,
                        limit: limit.unwrap_or(returned),
                        has_more,
                    }),
                );
            }
            (serde_json::Value::Object(obj.clone()), None)
        }
        serde_json::Value::Object(ref obj) if obj.contains_key("records") => {
            if let Some(serde_json::Value::Array(records)) = obj.get("records") {
                let total = records.len();
                let sliced: Vec<serde_json::Value> =
                    records.iter().skip(offset).take(take).cloned().collect();
                let returned = sliced.len();
                let has_more = offset + returned < total;
                let mut new_obj = obj.clone();
                new_obj.insert("records".into(), serde_json::Value::Array(sliced));
                return (
                    serde_json::Value::Object(new_obj),
                    Some(PaginationMeta {
                        total,
                        offset,
                        limit: limit.unwrap_or(returned),
                        has_more,
                    }),
                );
            }
            (serde_json::Value::Object(obj.clone()), None)
        }
        other => (other, None),
    }
}

// ── Serialization ────────────────────────────────────────────────────────────

fn serialize_to(value: &serde_json::Value, format: &str) -> Result<String, AgentOSError> {
    match format.to_lowercase().as_str() {
        "json" => {
            serde_json::to_string_pretty(value).map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!("JSON serialization failed: {}", e),
            })
        }
        "yaml" | "yml" => {
            serde_yaml::to_string(value).map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!("YAML serialization failed: {}", e),
            })
        }
        "toml" => {
            // Route through serde: JSON Value → TOML Value (both implement serde data model)
            let toml_val: toml::Value = serde_json::from_value(value.clone()).map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "data-parser".into(),
                    reason: format!("Cannot represent value as TOML: {}", e),
                }
            })?;
            toml::to_string_pretty(&toml_val).map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!("TOML serialization failed: {}", e),
            })
        }
        "csv" => serialize_to_delimited(value, b','),
        "tsv" => serialize_to_delimited(value, b'\t'),
        "jsonl" | "ndjson" => serialize_to_jsonl(value),
        "xml" => serialize_to_xml(value),
        "markdown" | "md" => serialize_to_markdown(value),
        other => Err(AgentOSError::SchemaValidation(format!(
            "Unsupported output_format: '{}'. Supported: json, yaml, toml, csv, tsv, jsonl, xml, markdown",
            other
        ))),
    }
}

/// Serialize to CSV or TSV. Uses the union of all row keys as headers so that
/// heterogeneous rows (different key sets) never silently drop columns.
fn serialize_to_delimited(
    value: &serde_json::Value,
    delimiter: u8,
) -> Result<String, AgentOSError> {
    let fmt = if delimiter == b'\t' { "TSV" } else { "CSV" };

    // Accept: array of objects, or {headers, rows} / {records} structures.
    let rows: Vec<&serde_json::Value> = match value {
        serde_json::Value::Array(arr) => arr.iter().collect(),
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::Array(r)) = obj.get("rows") {
                r.iter().collect()
            } else if let Some(serde_json::Value::Array(r)) = obj.get("records") {
                r.iter().collect()
            } else {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "data-parser".into(),
                    reason: format!(
                        "{} serialization requires an array or a {{rows}} / {{records}} object",
                        fmt
                    ),
                });
            }
        }
        _ => {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!(
                    "{} serialization requires an array or a {{rows}} / {{records}} object",
                    fmt
                ),
            });
        }
    };

    if rows.is_empty() {
        return Ok(String::new());
    }

    // Union of all keys across every row — preserves insertion order of first occurrence.
    let mut seen = std::collections::HashSet::new();
    let mut headers: Vec<String> = Vec::new();
    for row in &rows {
        if let serde_json::Value::Object(obj) = row {
            for key in obj.keys() {
                if seen.insert(key.clone()) {
                    headers.push(key.clone());
                }
            }
        } else {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "data-parser".into(),
                reason: format!("{} rows must be objects with string keys", fmt),
            });
        }
    }

    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(Vec::new());

    wtr.write_record(&headers)
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "data-parser".into(),
            reason: format!("{} write error: {}", fmt, e),
        })?;

    for row in &rows {
        if let serde_json::Value::Object(obj) = row {
            let fields: Vec<String> = headers
                .iter()
                .map(|h| match obj.get(h) {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                })
                .collect();
            wtr.write_record(&fields)
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "data-parser".into(),
                    reason: format!("{} write error: {}", fmt, e),
                })?;
        }
    }

    let bytes = wtr
        .into_inner()
        .map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "data-parser".into(),
            reason: format!("{} flush error: {}", fmt, e),
        })?;
    String::from_utf8(bytes).map_err(|_| AgentOSError::ToolExecutionFailed {
        tool_name: "data-parser".into(),
        reason: format!("{} output contains non-UTF-8 bytes", fmt),
    })
}

/// Serialize a value to JSONL. Arrays emit one line per element; scalar/object emits one line.
fn serialize_to_jsonl(value: &serde_json::Value) -> Result<String, AgentOSError> {
    // Unwrap {records} / {rows} wrappers that parsers produce.
    let items: Vec<&serde_json::Value> = match value {
        serde_json::Value::Array(arr) => arr.iter().collect(),
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::Array(r)) = obj.get("records") {
                r.iter().collect()
            } else if let Some(serde_json::Value::Array(r)) = obj.get("rows") {
                r.iter().collect()
            } else {
                vec![value]
            }
        }
        other => vec![other],
    };

    let mut out = String::new();
    for item in items {
        let line = serde_json::to_string(item).map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "data-parser".into(),
            reason: format!("JSONL serialization failed: {}", e),
        })?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// Serialize a JSON value to simple XML. Objects become elements; arrays become repeated
/// elements under a `<items>` root; scalars become `<value>` text nodes.
fn serialize_to_xml(value: &serde_json::Value) -> Result<String, AgentOSError> {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_xml_value(&mut out, "root", value);
    Ok(out)
}

fn write_xml_value(out: &mut String, tag: &str, value: &serde_json::Value) {
    fn escape_xml(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    match value {
        serde_json::Value::Object(obj) => {
            out.push_str(&format!("<{}>", tag));
            for (k, v) in obj {
                write_xml_value(out, k, v);
            }
            out.push_str(&format!("</{}>", tag));
        }
        serde_json::Value::Array(arr) => {
            out.push_str(&format!("<{}>", tag));
            for item in arr {
                write_xml_value(out, "item", item);
            }
            out.push_str(&format!("</{}>", tag));
        }
        serde_json::Value::String(s) => {
            out.push_str(&format!("<{}>{}</{}>", tag, escape_xml(s), tag));
        }
        serde_json::Value::Null => {
            out.push_str(&format!("<{} nil=\"true\"/>", tag));
        }
        other => {
            out.push_str(&format!("<{}>{}</{}>", tag, other, tag));
        }
    }
}

/// Serialize a value to Markdown. Objects become a key/value table; arrays of objects
/// become a table with union headers; scalars become a code block.
fn serialize_to_markdown(value: &serde_json::Value) -> Result<String, AgentOSError> {
    match value {
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return Ok(String::new());
            }
            // Collect union of all object keys to form table headers.
            let mut seen = std::collections::HashSet::new();
            let mut headers: Vec<String> = Vec::new();
            for item in arr {
                if let serde_json::Value::Object(obj) = item {
                    for k in obj.keys() {
                        if seen.insert(k.clone()) {
                            headers.push(k.clone());
                        }
                    }
                }
            }
            if headers.is_empty() {
                // Array of scalars → bullet list
                let mut out = String::new();
                for item in arr {
                    out.push_str(&format!("- {}\n", item));
                }
                return Ok(out);
            }
            // Table
            let mut out = format!("| {} |\n", headers.join(" | "));
            out.push_str(&format!(
                "|{}|\n",
                headers.iter().map(|_| "---|").collect::<String>()
            ));
            for item in arr {
                if let serde_json::Value::Object(obj) = item {
                    let cells: Vec<String> = headers
                        .iter()
                        .map(|h| match obj.get(h) {
                            Some(serde_json::Value::String(s)) => s.replace('|', "\\|"),
                            Some(v) => v.to_string(),
                            None => String::new(),
                        })
                        .collect();
                    out.push_str(&format!("| {} |\n", cells.join(" | ")));
                }
            }
            Ok(out)
        }
        serde_json::Value::Object(obj) => {
            // Key/value table
            let mut out = String::from("| Key | Value |\n|---|---|\n");
            for (k, v) in obj {
                let val = match v {
                    serde_json::Value::String(s) => s.replace('|', "\\|"),
                    other => other.to_string(),
                };
                out.push_str(&format!("| {} | {} |\n", k, val));
            }
            Ok(out)
        }
        other => Ok(format!("```\n{}\n```\n", other)),
    }
}
