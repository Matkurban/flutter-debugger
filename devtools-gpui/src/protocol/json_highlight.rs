use serde_json::Value;

/// Maximum payload size (256 KB) and line count (4,000 lines) for interactive syntax-colored
/// UI rendering to prevent large responses from stalling frame layout.
const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;
const MAX_HIGHLIGHT_LINES: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonTokenKind {
    Punctuation,
    Key,
    StringVal,
    NumberVal,
    BoolOrNull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonHighlightedLine {
    pub spans: Vec<(JsonTokenKind, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedBody {
    pub raw_len: usize,
    pub total_lines: usize,
    pub truncated: bool,
    pub full_copy_text: String,
    pub lines: Vec<JsonHighlightedLine>,
}

/// Pretty-prints a raw string if it is valid JSON (and within size bounds) and tokenizes lines
/// for syntax-colored rendering, capping visible lines at `MAX_HIGHLIGHT_LINES`.
pub fn format_and_tokenize_json(raw: &str) -> FormattedBody {
    let raw_len = raw.len();
    let pretty = if raw_len <= MAX_HIGHLIGHT_BYTES {
        match serde_json::from_str::<Value>(raw) {
            Ok(val) => serde_json::to_string_pretty(&val).unwrap_or_else(|_| raw.to_string()),
            Err(_) => raw.to_string(),
        }
    } else {
        raw.to_string()
    };

    let all_lines: Vec<&str> = pretty.lines().collect();
    let total_lines = all_lines.len();
    let truncated = raw_len > MAX_HIGHLIGHT_BYTES || total_lines > MAX_HIGHLIGHT_LINES;

    let display_slice = if total_lines > MAX_HIGHLIGHT_LINES {
        &all_lines[..MAX_HIGHLIGHT_LINES]
    } else {
        &all_lines[..]
    };

    let mut lines = Vec::with_capacity(display_slice.len());
    let mut byte_budget = 0usize;
    for &line in display_slice {
        if byte_budget + line.len() > MAX_HIGHLIGHT_BYTES {
            break;
        }
        byte_budget += line.len();
        lines.push(tokenize_pretty_json_line(line));
    }

    FormattedBody {
        raw_len,
        total_lines,
        truncated,
        full_copy_text: pretty,
        lines,
    }
}

fn tokenize_pretty_json_line(line: &str) -> JsonHighlightedLine {
    let trimmed = line.trim_start();
    let indent_len = line.len() - trimmed.len();
    let indent = &line[..indent_len];

    if let Some(after_quote) = trimmed.strip_prefix('"')
        && let Some(end_quote_idx) = find_unescaped_quote(after_quote)
    {
        let key_part = &trimmed[..end_quote_idx + 2];
        let rest = &trimmed[end_quote_idx + 2..];
        if let Some(after_colon) = rest.strip_prefix(": ") {
            let mut spans = vec![
                (JsonTokenKind::Punctuation, indent.to_string()),
                (JsonTokenKind::Key, key_part.to_string()),
                (JsonTokenKind::Punctuation, ": ".to_string()),
            ];
            spans.extend(tokenize_json_value_tail(after_colon));
            return JsonHighlightedLine { spans };
        }
    }

    let mut spans = Vec::new();
    if !indent.is_empty() {
        spans.push((JsonTokenKind::Punctuation, indent.to_string()));
    }
    spans.extend(tokenize_json_value_tail(trimmed));
    JsonHighlightedLine { spans }
}

fn find_unescaped_quote(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, ch) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            return Some(i);
        }
    }
    None
}

fn tokenize_json_value_tail(val_str: &str) -> Vec<(JsonTokenKind, String)> {
    let (core, trailing_comma) = match val_str.strip_suffix(',') {
        Some(c) => (c, true),
        None => (val_str, false),
    };

    let kind = if core.starts_with('"') {
        JsonTokenKind::StringVal
    } else if matches!(core, "true" | "false" | "null") {
        JsonTokenKind::BoolOrNull
    } else if core
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == '-')
    {
        JsonTokenKind::NumberVal
    } else {
        JsonTokenKind::Punctuation
    };

    let mut out = vec![(kind, core.to_string())];
    if trailing_comma {
        out.push((JsonTokenKind::Punctuation, ",".to_string()));
    }
    out
}
