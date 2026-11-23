use base64::Engine as _;
use serde_json::Value;

use crate::protocol::inspector::SourceLocation;
use crate::theme::{current_local_hms, format_epoch_millis_local_hms};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Stdout,
    Stderr,
    Log,
    FlutterError,
    FlutterInfo,
    Gc,
}

impl LogKind {
    pub fn badge_label(self) -> &'static str {
        match self {
            Self::Stdout => "STDOUT",
            Self::Stderr => "STDERR",
            Self::Log => "LOG",
            Self::FlutterError => "ERROR",
            Self::FlutterInfo => "FLUTTER",
            Self::Gc => "GC",
        }
    }

    pub fn is_error(self) -> bool {
        matches!(self, Self::Stderr | Self::FlutterError)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TruncatedLogRefs {
    pub message_object_id: Option<String>,
    pub error_object_id: Option<String>,
    pub stack_object_id: Option<String>,
}

impl TruncatedLogRefs {
    pub fn has_any(&self) -> bool {
        self.message_object_id.is_some()
            || self.error_object_id.is_some()
            || self.stack_object_id.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLogRecord {
    pub entry: LogEntry,
    pub truncated: TruncatedLogRefs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub seq: u64,
    pub timestamp_hms: String,
    pub kind: LogKind,
    pub logger_name: String,
    pub summary: String,
    pub full_text: String,
    pub stack_trace: Option<String>,
    pub source_refs: Vec<SourceLocation>,
}

impl LogEntry {
    pub fn from_text(
        seq: u64,
        kind: LogKind,
        logger_name: &str,
        text: String,
        stack: Option<String>,
    ) -> Self {
        Self::from_text_with_time(
            seq,
            current_local_hms(),
            kind,
            logger_name,
            text,
            stack,
        )
    }

    pub fn from_text_with_time(
        seq: u64,
        timestamp_hms: String,
        kind: LogKind,
        logger_name: &str,
        text: String,
        stack: Option<String>,
    ) -> Self {
        let summary = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        let mut source_refs = extract_source_locations_from_text(&text);
        if let Some(ref st) = stack {
            for loc in extract_source_locations_from_text(st) {
                if !source_refs.contains(&loc) {
                    source_refs.push(loc);
                }
            }
        }
        Self {
            seq,
            timestamp_hms,
            kind,
            logger_name: logger_name.to_string(),
            summary: if summary.is_empty() {
                text.trim().to_string()
            } else {
                summary
            },
            full_text: text,
            stack_trace: stack,
            source_refs,
        }
    }

    /// Parses a Dart VM `Logging` stream `LogRecord` event, extracting local timestamp from `logRecord.time`
    /// and detecting `valueAsStringIsTruncated == true` `InstanceRef` object IDs for async expansion.
    pub fn parse_logging_stream_record(event: &Value, seq: u64) -> Option<ParsedLogRecord> {
        let rec = event.get("logRecord")?;
        let msg_obj = rec.get("message");
        let message = msg_obj
            .and_then(|m| m.get("valueAsString"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if message.is_empty() {
            return None;
        }

        let logger_name = rec
            .get("loggerName")
            .and_then(|m| m.get("valueAsString"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("dart:developer")
            .to_string();

        let level = rec.get("level").and_then(Value::as_i64).unwrap_or(0);
        let timestamp_hms = rec
            .get("time")
            .and_then(Value::as_u64)
            .filter(|&ms| ms > 0)
            .map(|ms| format_epoch_millis_local_hms(ms, false))
            .unwrap_or_else(current_local_hms);

        let err_obj = rec.get("error");
        let error_str = err_obj
            .and_then(|m| m.get("valueAsString"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "null")
            .map(ToOwned::to_owned);

        let stack_obj = rec.get("stackTrace");
        let stack_str = stack_obj
            .and_then(|m| m.get("valueAsString"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "null")
            .map(ToOwned::to_owned);

        let truncated = TruncatedLogRefs {
            message_object_id: extract_truncated_object_id(msg_obj),
            error_object_id: extract_truncated_object_id(err_obj),
            stack_object_id: extract_truncated_object_id(stack_obj),
        };

        let kind = if level >= 1000 || error_str.is_some() {
            LogKind::FlutterError
        } else {
            LogKind::Log
        };

        let full_text = match error_str {
            Some(err) => format!("{message}\nError: {err}"),
            None => message,
        };

        let entry = Self::from_text_with_time(
            seq,
            timestamp_hms,
            kind,
            &logger_name,
            full_text,
            stack_str,
        );
        Some(ParsedLogRecord { entry, truncated })
    }

    /// Parses an official `Flutter.Error` extension event (`DiagnosticsNode` JSON).
    /// Prioritizes `renderedErrorText`, then inspects `properties` for `StackTrace` or child descriptions,
    /// falling back to `description`.
    pub fn parse_flutter_error_extension(ext_data: &Value, seq: u64) -> Self {
        let rendered = ext_data
            .get("renderedErrorText")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned);

        let desc = ext_data
            .get("description")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Flutter framework error")
            .to_string();

        let mut stack = ext_data
            .get("stackTrace")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned);

        if stack.is_none()
            && let Some(props) = ext_data.get("properties").and_then(Value::as_array)
        {
            for prop in props {
                let ptype = prop
                    .get("propertyType")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let pdesc = prop
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if (ptype == "StackTrace" || pdesc.contains("#0 ")) && !pdesc.trim().is_empty() {
                    stack = Some(pdesc.to_string());
                    break;
                }
            }
        }

        let full_text = rendered.unwrap_or(desc);
        Self::from_text(seq, LogKind::FlutterError, "Flutter.Error", full_text, stack)
    }

    /// Decodes a Base64-encoded `WriteEvent` from `Stdout` or `Stderr` stream using the `base64` crate.
    pub fn decode_write_event_lines(stream_id: &str, b64_bytes: &str) -> Vec<(LogKind, String)> {
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64_bytes.trim()) else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&decoded);
        let kind = if stream_id == "Stderr" {
            LogKind::Stderr
        } else {
            LogKind::Stdout
        };
        text.lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .map(|l| (kind, l.to_string()))
            .collect()
    }
}

fn extract_truncated_object_id(instance_ref: Option<&Value>) -> Option<String> {
    let obj = instance_ref?;
    if obj.get("valueAsStringIsTruncated").and_then(Value::as_bool) == Some(true) {
        obj.get("id").and_then(Value::as_str).map(ToOwned::to_owned)
    } else {
        None
    }
}

/// Extracts `package:foo/bar.dart:12:34` or `file:///path/to/bar.dart:12:34` references from log/stack lines.
pub fn extract_source_locations_from_text(text: &str) -> Vec<SourceLocation> {
    let mut locations = Vec::new();
    for token in
        text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | ','))
    {
        let trimmed = token.trim_matches(|c: char| matches!(c, '\'' | '"' | ';'));
        if (trimmed.starts_with("package:") || trimmed.starts_with("file://"))
            && let Some(loc) = parse_dart_uri_with_line_col(trimmed)
            && !locations.contains(&loc)
        {
            locations.push(loc);
        }
    }
    locations
}

pub fn parse_dart_uri_with_line_col(token: &str) -> Option<SourceLocation> {
    let dart_idx = token.find(".dart")?;
    let uri_end = dart_idx + ".dart".len();
    let uri = token[..uri_end].to_string();
    let tail = &token[uri_end..];
    let mut nums = tail.trim_start_matches(':').split(':');
    let line = nums.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
    let column = nums.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
    Some(SourceLocation { uri, line, column })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_official_flutter_error_with_rendered_error_text_and_properties_stack() {
        let ext_data = json!({
            "type": "DiagnosticsNode",
            "description": "A RenderFlex overflowed by 42 pixels on the right.",
            "renderedErrorText": "══╡ EXCEPTION CAUGHT BY RENDERING LIBRARY ╞══\nThe following assertion was thrown during layout:\nA RenderFlex overflowed by 42 pixels on the right.\nThe relevant error-causing widget was:\n  Row file:///Users/dev/my_app/lib/home_page.dart:88:12",
            "properties": [
                {
                    "description": "#0 RenderFlex.performLayout (package:flutter/src/rendering/flex.dart:1024:14)\n#1 HomePage.build (package:my_app/home_page.dart:88:12)",
                    "propertyType": "StackTrace"
                }
            ]
        });

        let entry = LogEntry::parse_flutter_error_extension(&ext_data, 1);
        assert_eq!(entry.kind, LogKind::FlutterError);
        assert!(entry.full_text.contains("EXCEPTION CAUGHT BY RENDERING LIBRARY"));
        assert!(entry.stack_trace.as_deref().unwrap().contains("#0 RenderFlex.performLayout"));
        assert_eq!(entry.source_refs.len(), 3);
    }

    #[test]
    fn detects_truncated_log_record_and_parses_timestamp() {
        let ev = json!({
            "logRecord": {
                "time": 1_700_000_000_000u64,
                "level": 800,
                "loggerName": { "valueAsString": "api" },
                "message": {
                    "id": "objects/msg-99",
                    "valueAsString": "Large JSON response...",
                    "valueAsStringIsTruncated": true
                },
                "error": { "valueAsString": "null" },
                "stackTrace": {
                    "id": "objects/stack-100",
                    "valueAsString": "#0 ApiClient.fetch (package:my_app/api.dart:19:5)...",
                    "valueAsStringIsTruncated": true
                }
            }
        });

        let parsed = LogEntry::parse_logging_stream_record(&ev, 5).expect("parse record");
        assert_eq!(parsed.entry.seq, 5);
        assert_eq!(
            parsed.truncated.message_object_id.as_deref(),
            Some("objects/msg-99")
        );
        assert_eq!(
            parsed.truncated.stack_object_id.as_deref(),
            Some("objects/stack-100")
        );
        assert!(parsed.truncated.has_any());
    }
}
