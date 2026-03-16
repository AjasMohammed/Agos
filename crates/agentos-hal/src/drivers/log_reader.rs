use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::hal::HalDriver;
use crate::types::{LogEntry, LogLevel, LogQuery, LogSource};
use regex::Regex;

pub struct LogReaderDriver {
    app_log_paths: HashMap<String, PathBuf>,
    system_log_paths: HashMap<String, PathBuf>,
}

impl LogReaderDriver {
    pub fn new(app_logs: HashMap<String, PathBuf>, system_logs: HashMap<String, PathBuf>) -> Self {
        Self {
            app_log_paths: app_logs,
            system_log_paths: system_logs,
        }
    }

    async fn read_log_file(
        &self,
        path: &Path,
        query: &LogQuery,
    ) -> Result<Vec<LogEntry>, AgentOSError> {
        let file = File::open(path).await.map_err(|e| {
            AgentOSError::HalError(format!("Failed to open log file {}: {}", path.display(), e))
        })?;

        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        let mut entries = Vec::new();
        const MAX_REGEX_LEN: usize = 1024;
        const MAX_LOG_ENTRIES: usize = 50_000;

        let grep_regex = match query.grep_pattern.as_ref() {
            Some(pattern) => {
                if pattern.len() > MAX_REGEX_LEN {
                    return Err(AgentOSError::HalError(format!(
                        "Grep pattern too long: {} bytes (max {})",
                        pattern.len(),
                        MAX_REGEX_LEN
                    )));
                }
                Some(Regex::new(pattern).map_err(|e| {
                    AgentOSError::HalError(format!("Invalid grep pattern '{}': {}", pattern, e))
                })?)
            }
            None => None,
        };

        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let (timestamp, level) = parse_log_line_metadata(&line);

                    let mut include = true;
                    if let Some(ref re) = grep_regex {
                        if !re.is_match(&line) {
                            include = false;
                        }
                    }

                    if let Some(ref levels) = query.level_filter {
                        if !levels.is_empty() && !levels.contains(&level) {
                            include = false;
                        }
                    }

                    if let Some(since) = query.since {
                        if timestamp < since {
                            include = false;
                        }
                    }

                    if include {
                        entries.push(LogEntry {
                            timestamp,
                            level,
                            message: line,
                            source: path.to_string_lossy().to_string(),
                        });
                        // Cap total entries to prevent OOM on large log files
                        if entries.len() >= MAX_LOG_ENTRIES {
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(AgentOSError::HalError(format!(
                        "Error reading log file {}: {}",
                        path.display(),
                        e
                    )));
                }
            }
        }

        if let Some(last_n) = query.last_n_lines {
            let skip = entries.len().saturating_sub(last_n as usize);
            entries = entries.into_iter().skip(skip).collect();
        }

        Ok(entries)
    }
}

/// Parse timestamp and log level from a log line.
/// Recognizes common formats like "2024-01-15T10:30:00Z [INFO] ..." or "2024-01-15 10:30:00 ERROR ...".
/// Falls back to current time and Info level if parsing fails.
fn parse_log_line_metadata(line: &str) -> (chrono::DateTime<chrono::Utc>, LogLevel) {
    let trimmed = line.trim();

    // Try to extract timestamp from the beginning (ISO 8601 / RFC 3339)
    let timestamp = try_parse_timestamp(trimmed).unwrap_or_else(chrono::Utc::now);

    // Try to extract log level from common patterns
    let level = parse_log_level(trimmed).unwrap_or(LogLevel::Info);

    (timestamp, level)
}

fn try_parse_timestamp(line: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // Try first ~35 chars for a timestamp (safe UTF-8 boundary)
    let prefix = if line.len() > 35 {
        // Find the last valid char boundary at or before byte 35
        let end = (0..=35)
            .rev()
            .find(|&i| line.is_char_boundary(i))
            .unwrap_or(0);
        &line[..end]
    } else {
        line
    };

    // Try RFC 3339 / ISO 8601
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(prefix.split_whitespace().next()?) {
        return Some(dt.with_timezone(&chrono::Utc));
    }

    // Try "YYYY-MM-DD HH:MM:SS" format
    if prefix.len() >= 19 {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&prefix[..19], "%Y-%m-%d %H:%M:%S") {
            return Some(dt.and_utc());
        }
    }

    None
}

fn parse_log_level(line: &str) -> Option<LogLevel> {
    let upper = line.to_uppercase();
    // Check for bracketed levels like [INFO], [ERROR], etc. or bare labels
    for (label, level) in [
        ("ERROR", LogLevel::Error),
        ("WARN", LogLevel::Warn),
        ("WARNING", LogLevel::Warn),
        ("INFO", LogLevel::Info),
        ("DEBUG", LogLevel::Debug),
    ] {
        if upper.contains(label) {
            return Some(level);
        }
    }
    None
}

#[async_trait]
impl HalDriver for LogReaderDriver {
    fn name(&self) -> &str {
        "log"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("fs.app_logs", PermissionOp::Read)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let query: LogQuery = serde_json::from_value(params)
            .map_err(|e| AgentOSError::HalError(format!("Invalid LogQuery params: {}", e)))?;

        let path = match &query.source {
            LogSource::AppLog(name) => self.app_log_paths.get(name).ok_or_else(|| {
                AgentOSError::HalError(format!("App log source '{}' not configured", name))
            })?,
            LogSource::SystemLog => self.system_log_paths.get("syslog").ok_or_else(|| {
                AgentOSError::HalError("System log source 'syslog' not configured".to_string())
            })?,
            LogSource::KernelLog => self.system_log_paths.get("kernlog").ok_or_else(|| {
                AgentOSError::HalError("Kernel log source 'kernlog' not configured".to_string())
            })?,
        };

        let entries = self.read_log_file(path, &query).await?;
        Ok(serde_json::to_value(entries).map_err(|e| AgentOSError::HalError(e.to_string()))?)
    }
}
