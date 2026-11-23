use tower_lsp_server::ls_types::{InlayHint, InlayHintKind, InlayHintLabel, Position};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DartTestEntry {
    pub line: u32,
    /// Local name passed to `test(...)` / `group(...)`.
    pub local_name: String,
    /// Full space-joined path including enclosing `group(...)` names for `--plain-name`.
    pub full_name: String,
    pub is_group: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosingLabelEntry {
    pub label: String,
    pub open_line: u32,
    pub close_line: u32,
    pub close_col_utf16: u32,
}

#[derive(Debug, Clone, Default)]
pub struct DartSyntaxInfo {
    pub main_line: Option<u32>,
    pub tests: Vec<DartTestEntry>,
    pub closing_labels: Vec<ClosingLabelEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Ident(String),
    StringLit(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Dot,
    Comma,
    Semicolon,
    Colon,
    FatArrow,
    LAngle,
    RAngle,
    Other,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    line: u32,
    end_col_utf16: u32,
    byte_end: usize,
}

const TEST_FN_NAMES: &[&str] = &[
    "test",
    "testWidgets",
    "testGoldens",
    "patrolTest",
    "blocTest",
    "group",
];

const CONTROL_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "catch", "return", "throw", "await", "yield", "assert",
    "super", "this", "new", "const", "class", "enum", "mixin", "extension", "typedef", "void",
    "Function", "Future", "Stream", "List", "Map", "Set", "Iterable", "Object", "Dynamic",
];

/// Replaces all Dart comments (`//`, `/* ... */`) and string literal contents with ASCII spaces
/// while preserving exact byte offsets and newline (`\n`) positions.
/// Used by `colors.rs` so color regexes never false-match inside comments or string literals.
pub fn mask_dart_comments_and_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut out = Vec::with_capacity(len);
    let mut i = 0usize;

    #[derive(Clone, Copy)]
    struct StrState {
        quote: u8,
        triple: bool,
        raw: bool,
        interp_brace_depth: u32,
    }
    let mut str_stack: Vec<StrState> = Vec::new();

    while i < len {
        // Inside a string literal (not currently inside `${...}` interpolation)
        if let Some(top) = str_stack.last().copied()
            && top.interp_brace_depth == 0
        {
            if top.triple {
                if i + 2 < len
                    && bytes[i] == top.quote
                    && bytes[i + 1] == top.quote
                    && bytes[i + 2] == top.quote
                {
                    out.extend_from_slice(b"   ");
                    i += 3;
                    str_stack.pop();
                    continue;
                }
            } else if bytes[i] == top.quote {
                out.push(b' ');
                i += 1;
                str_stack.pop();
                continue;
            }

            if !top.raw && bytes[i] == b'\\' && i + 1 < len {
                let next_b = bytes[i + 1];
                out.push(b' ');
                out.push(if next_b == b'\n' { b'\n' } else { b' ' });
                i += 2;
                continue;
            }

            if !top.raw && bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                str_stack.last_mut().unwrap().interp_brace_depth = 1;
                out.extend_from_slice(b"  ");
                i += 2;
                continue;
            }

            let b = bytes[i];
            out.push(if b == b'\n' { b'\n' } else { b' ' });
            i += 1;
            continue;
        }

        let b = bytes[i];

        // Closing `}` of `${...}` interpolation
        if b == b'}'
            && let Some(top) = str_stack.last_mut()
            && top.interp_brace_depth > 0
        {
            top.interp_brace_depth -= 1;
            out.push(if top.interp_brace_depth == 0 {
                b' '
            } else {
                b'}'
            });
            i += 1;
            continue;
        }

        if b == b'{'
            && let Some(top) = str_stack.last_mut()
            && top.interp_brace_depth > 0
        {
            top.interp_brace_depth += 1;
            out.push(b'{');
            i += 1;
            continue;
        }

        // Line comment `// ...`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            while i < len && bytes[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
            continue;
        }

        // Nested block comment `/* ... */`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            out.extend_from_slice(b"  ");
            i += 2;
            let mut depth = 1u32;
            while i < len && depth > 0 {
                if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
                    depth += 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                    depth -= 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else {
                    out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
            continue;
        }

        // Start of raw or regular string literal
        let is_raw_str =
            b == b'r' && i + 1 < len && (bytes[i + 1] == b'\'' || bytes[i + 1] == b'"');
        if is_raw_str || b == b'\'' || b == b'"' {
            if is_raw_str {
                out.push(b' ');
                i += 1;
            }
            let quote = bytes[i];
            let triple = i + 2 < len && bytes[i + 1] == quote && bytes[i + 2] == quote;
            if triple {
                out.extend_from_slice(b"   ");
                i += 3;
            } else {
                out.push(b' ');
                i += 1;
            }
            str_stack.push(StrState {
                quote,
                triple,
                raw: is_raw_str,
                interp_brace_depth: 0,
            });
            continue;
        }

        out.push(b);
        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

/// Parses a Dart source file in a single fast lexer + balanced-delimiter pass (< 1ms).
pub fn parse_dart_syntax(content: &str, scan_tests: bool) -> DartSyntaxInfo {
    let tokens = tokenize_dart(content);
    let lines: Vec<&str> = content.lines().collect();

    let mut match_close: Vec<Option<usize>> = vec![None; tokens.len()];
    let mut stack: Vec<(usize, TokenKind)> = Vec::new();
    for (idx, tok) in tokens.iter().enumerate() {
        match tok.kind {
            TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => {
                stack.push((idx, tok.kind.clone()));
            }
            TokenKind::RParen => {
                if let Some((open_idx, TokenKind::LParen)) = stack.last() {
                    match_close[*open_idx] = Some(idx);
                    stack.pop();
                }
            }
            TokenKind::RBrace => {
                if let Some((open_idx, TokenKind::LBrace)) = stack.last() {
                    match_close[*open_idx] = Some(idx);
                    stack.pop();
                }
            }
            TokenKind::RBracket => {
                if let Some((open_idx, TokenKind::LBracket)) = stack.last() {
                    match_close[*open_idx] = Some(idx);
                    stack.pop();
                }
            }
            _ => {}
        }
    }

    let mut info = DartSyntaxInfo::default();
    let mut group_stack: Vec<(usize, String)> = Vec::new();
    let mut brace_depth: u32 = 0;

    for i in 0..tokens.len() {
        while let Some(&(end_idx, _)) = group_stack.last() {
            if i > end_idx {
                group_stack.pop();
            } else {
                break;
            }
        }

        match tokens[i].kind {
            TokenKind::LBrace => {
                brace_depth += 1;
                continue;
            }
            TokenKind::RBrace => {
                brace_depth = brace_depth.saturating_sub(1);
                continue;
            }
            _ => {}
        }

        let TokenKind::Ident(ref name) = tokens[i].kind else {
            continue;
        };

        // 1. Detect top-level `main(` ONLY at `brace_depth == 0`
        if info.main_line.is_none() && brace_depth == 0 && name == "main" {
            let next_is_lparen = tokens
                .get(i + 1)
                .is_some_and(|t| matches!(t.kind, TokenKind::LParen));
            let prev_is_dot = i > 0 && matches!(tokens[i - 1].kind, TokenKind::Dot);
            if next_is_lparen && !prev_is_dot {
                info.main_line = Some(tokens[i].line);
            }
        }

        // 2. Detect `test` / `testWidgets` / `group` / `testGoldens` / `patrolTest` / `blocTest`
        if scan_tests && TEST_FN_NAMES.contains(&name.as_str()) {
            let prev_is_dot = i > 0 && matches!(tokens[i - 1].kind, TokenKind::Dot);
            if !prev_is_dot {
                let lparen_idx = skip_optional_type_args(&tokens, i + 1);
                if let Some(open_idx) = lparen_idx
                    && matches!(
                        tokens.get(open_idx).map(|t| &t.kind),
                        Some(TokenKind::LParen)
                    )
                    && let Some(close_idx) = match_close[open_idx]
                    && let Some(first_str) =
                        extract_first_string_arg(&tokens, open_idx + 1, close_idx)
                {
                    let is_group = name == "group";
                    let mut parts: Vec<&str> =
                        group_stack.iter().map(|(_, g)| g.as_str()).collect();
                    parts.push(&first_str);
                    let full_name = parts.join(" ");

                    info.tests.push(DartTestEntry {
                        line: tokens[i].line,
                        local_name: first_str.clone(),
                        full_name,
                        is_group,
                    });

                    if is_group {
                        group_stack.push((close_idx, first_str));
                    }
                }
            }
        }

        // 3. Detect Flutter Widget constructor calls for Closing Labels
        if is_pascal_case(name) && !CONTROL_KEYWORDS.contains(&name.as_str()) {
            let prev_is_dot = i > 0 && matches!(tokens[i - 1].kind, TokenKind::Dot);
            if prev_is_dot {
                let prefix_is_lower = i >= 2
                    && matches!(&tokens[i - 2].kind, TokenKind::Ident(p) if p.chars().next().is_some_and(|c| c.is_ascii_lowercase()));
                if !prefix_is_lower {
                    continue;
                }
            }

            if i > 0
                && matches!(
                    &tokens[i - 1].kind,
                    TokenKind::Ident(prev)
                        if matches!(
                            prev.as_str(),
                            "class" | "extends" | "implements" | "with" | "mixin" | "enum" | "typedef"
                        )
                )
            {
                continue;
            }

            let mut label = name.clone();
            let mut cursor = i + 1;

            if let Some(after_generics) = skip_optional_type_args(&tokens, cursor) {
                cursor = after_generics;
            }

            if cursor + 1 < tokens.len()
                && matches!(tokens[cursor].kind, TokenKind::Dot)
                && let TokenKind::Ident(ref ctor) = tokens[cursor + 1].kind
            {
                label.push('.');
                label.push_str(ctor);
                cursor += 2;
            }

            if cursor < tokens.len()
                && matches!(tokens[cursor].kind, TokenKind::LParen)
                && let Some(close_idx) = match_close[cursor]
            {
                let open_line = tokens[cursor].line;
                let close_tok = &tokens[close_idx];
                let close_line = close_tok.line;

                if close_line > open_line {
                    let next_kind = tokens.get(close_idx + 1).map(|t| &t.kind);
                    // Skip constructor initializer lists `: super(...)`, block bodies `{`, and arrow bodies `=>`
                    let is_definition = matches!(
                        next_kind,
                        Some(TokenKind::LBrace | TokenKind::Colon | TokenKind::FatArrow)
                    ) || matches!(
                        next_kind,
                        Some(TokenKind::Ident(kw)) if matches!(kw.as_str(), "async" | "sync" | "super")
                    );

                    // String-aware check for real `//` comment on the closing line (won't misfire on `"https://..."`)
                    let line_has_comment = lines
                        .get(close_line as usize)
                        .is_some_and(|l| line_has_real_line_comment(l));

                    if !is_definition && !line_has_comment {
                        // Advance past all trailing closing delimiters/punctuation on the same line
                        // (e.g. `)),` -> place label cleanly at end of line after `,`)
                        let mut hint_col = close_tok.end_col_utf16;
                        let mut scan = close_idx + 1;
                        while let Some(after) = tokens.get(scan) {
                            if after.line != close_line {
                                break;
                            }
                            if matches!(
                                after.kind,
                                TokenKind::RParen
                                    | TokenKind::RBracket
                                    | TokenKind::Comma
                                    | TokenKind::Semicolon
                            ) {
                                hint_col = after.end_col_utf16;
                                scan += 1;
                            } else {
                                break;
                            }
                        }

                        let rest_of_line = content
                            .get(close_tok.byte_end..)
                            .and_then(|s| s.lines().next())
                            .unwrap_or_default()
                            .trim();
                        let ends_cleanly = rest_of_line
                            .chars()
                            .all(|c| matches!(c, ')' | ']' | ',' | ';' | ' ' | '\t' | '\r'));

                        if ends_cleanly {
                            info.closing_labels.push(ClosingLabelEntry {
                                label,
                                open_line,
                                close_line,
                                close_col_utf16: hint_col,
                            });
                        }
                    }
                }
            }
        }
    }

    info
}

pub fn build_closing_label_inlay_hints(
    labels: &[ClosingLabelEntry],
    min_lines: u32,
) -> Vec<InlayHint> {
    let threshold = min_lines.max(1);
    let mut seen_lines = std::collections::HashSet::new();
    let mut hints = Vec::new();

    for entry in labels {
        if entry.close_line.saturating_sub(entry.open_line) < threshold {
            continue;
        }
        if !seen_lines.insert(entry.close_line) {
            continue;
        }
        hints.push(InlayHint {
            position: Position::new(entry.close_line, entry.close_col_utf16),
            label: InlayHintLabel::String(format!(" // {}", entry.label)),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: Some(true),
            padding_right: Some(false),
            data: None,
        });
    }

    hints
}

fn line_has_real_line_comment(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if (in_single || in_double) && b == b'\\' {
            i += 2;
            continue;
        }
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'/' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn is_pascal_case(ident: &str) -> bool {
    let mut chars = ident.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase() && chars.any(|c| c.is_ascii_lowercase())
}

fn skip_optional_type_args(tokens: &[Token], start: usize) -> Option<usize> {
    if start >= tokens.len() {
        return None;
    }
    if !matches!(tokens[start].kind, TokenKind::LAngle) {
        return Some(start);
    }
    let mut depth = 0i32;
    for (idx, tok) in tokens.iter().enumerate().skip(start) {
        match tok.kind {
            TokenKind::LAngle => depth += 1,
            TokenKind::RAngle => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx + 1);
                }
            }
            TokenKind::Semicolon | TokenKind::LBrace => return None,
            _ => {}
        }
    }
    None
}

fn extract_first_string_arg(tokens: &[Token], start: usize, end: usize) -> Option<String> {
    let mut combined = String::new();
    let mut found = false;
    for tok in tokens.iter().take(end).skip(start) {
        match &tok.kind {
            TokenKind::StringLit(s) => {
                combined.push_str(s);
                found = true;
            }
            TokenKind::Comma | TokenKind::RParen => break,
            _ if found => break,
            _ => return None,
        }
    }
    if found && !combined.is_empty() {
        Some(combined)
    } else {
        None
    }
}

/// Fast, comment-aware, string-interpolation-aware (`${...}`) Dart tokenizer tracking UTF-16 columns.
fn tokenize_dart(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut line = 0u32;
    let mut col_utf16 = 0u32;
    let mut tokens = Vec::with_capacity(src.len() / 6);

    #[derive(Clone)]
    struct ActiveString {
        quote: u8,
        triple: bool,
        raw: bool,
        start_line: u32,
        value: String,
        interp_brace_depth: u32,
    }
    let mut str_stack: Vec<ActiveString> = Vec::new();

    while i < len {
        // Continue scanning a string literal if we're not inside `${...}` interpolation
        if let Some(top) = str_stack.last().map(|s| s.interp_brace_depth)
            && top == 0
        {
            let active = str_stack.last_mut().unwrap();
            let quote = active.quote;
            let triple = active.triple;
            let raw = active.raw;

            if triple {
                if i + 2 < len
                    && bytes[i] == quote
                    && bytes[i + 1] == quote
                    && bytes[i + 2] == quote
                {
                    i += 3;
                    col_utf16 += 3;
                    let finished = str_stack.pop().unwrap();
                    tokens.push(Token {
                        kind: TokenKind::StringLit(finished.value),
                        line: finished.start_line,
                        end_col_utf16: col_utf16,
                        byte_end: i,
                    });
                    continue;
                }
            } else if bytes[i] == quote {
                i += 1;
                col_utf16 += 1;
                let finished = str_stack.pop().unwrap();
                tokens.push(Token {
                    kind: TokenKind::StringLit(finished.value),
                    line: finished.start_line,
                    end_col_utf16: col_utf16,
                    byte_end: i,
                });
                continue;
            }

            if !raw && bytes[i] == b'\\' && i + 1 < len {
                let esc = src[i + 1..].chars().next().unwrap();
                active.value.push(esc);
                i += 1 + esc.len_utf8();
                if esc == '\n' {
                    line += 1;
                    col_utf16 = 0;
                } else if esc != '\r' {
                    col_utf16 += 1 + esc.len_utf16() as u32;
                }
                continue;
            }

            // Enter `${...}` string interpolation
            if !raw && bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                active.interp_brace_depth = 1;
                i += 2;
                col_utf16 += 2;
                continue;
            }

            let ch = src[i..].chars().next().unwrap();
            if ch == '\n' {
                line += 1;
                col_utf16 = 0;
                active.value.push('\n');
            } else if ch != '\r' {
                col_utf16 += ch.len_utf16() as u32;
                active.value.push(ch);
            }
            i += ch.len_utf8();
            continue;
        }

        let b = bytes[i];

        // Whitespace (`\r` does NOT increment UTF-16 column)
        if b == b'\n' {
            line += 1;
            col_utf16 = 0;
            i += 1;
            continue;
        }
        if b == b'\r' {
            i += 1;
            continue;
        }
        if b == b' ' || b == b'\t' {
            col_utf16 += 1;
            i += 1;
            continue;
        }

        // Line comment `// ...`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Nested block comment `/* ... */`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            i += 2;
            col_utf16 += 2;
            let mut depth = 1u32;
            while i < len && depth > 0 {
                if bytes[i] == b'\n' {
                    line += 1;
                    col_utf16 = 0;
                    i += 1;
                } else if bytes[i] == b'\r' {
                    i += 1;
                } else if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                    col_utf16 += 2;
                } else if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    col_utf16 += 2;
                } else {
                    let ch = src[i..].chars().next().unwrap();
                    i += ch.len_utf8();
                    col_utf16 += ch.len_utf16() as u32;
                }
            }
            continue;
        }

        // Track `{` / `}` inside `${...}` interpolation
        if b == b'{'
            && let Some(top) = str_stack.last_mut()
            && top.interp_brace_depth > 0
        {
            top.interp_brace_depth += 1;
            i += 1;
            col_utf16 += 1;
            tokens.push(Token {
                kind: TokenKind::LBrace,
                line,
                end_col_utf16: col_utf16,
                byte_end: i,
            });
            continue;
        }
        if b == b'}'
            && let Some(top) = str_stack.last_mut()
            && top.interp_brace_depth > 0
        {
            top.interp_brace_depth -= 1;
            i += 1;
            col_utf16 += 1;
            if top.interp_brace_depth > 0 {
                tokens.push(Token {
                    kind: TokenKind::RBrace,
                    line,
                    end_col_utf16: col_utf16,
                    byte_end: i,
                });
            }
            continue;
        }

        // Start of string literal (raw or regular)
        let is_raw_str =
            b == b'r' && i + 1 < len && (bytes[i + 1] == b'\'' || bytes[i + 1] == b'"');
        if is_raw_str || b == b'\'' || b == b'"' {
            let start_line = line;
            if is_raw_str {
                i += 1;
                col_utf16 += 1;
            }
            let quote = bytes[i];
            let triple = i + 2 < len && bytes[i + 1] == quote && bytes[i + 2] == quote;
            if triple {
                i += 3;
                col_utf16 += 3;
            } else {
                i += 1;
                col_utf16 += 1;
            }
            str_stack.push(ActiveString {
                quote,
                triple,
                raw: is_raw_str,
                start_line,
                value: String::new(),
                interp_brace_depth: 0,
            });
            continue;
        }

        // Fat arrow `=>`
        if b == b'=' && i + 1 < len && bytes[i + 1] == b'>' {
            i += 2;
            col_utf16 += 2;
            tokens.push(Token {
                kind: TokenKind::FatArrow,
                line,
                end_col_utf16: col_utf16,
                byte_end: i,
            });
            continue;
        }

        // Identifiers
        if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
            let start = i;
            let start_line = line;
            while i < len
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
                col_utf16 += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Ident(src[start..i].to_string()),
                line: start_line,
                end_col_utf16: col_utf16,
                byte_end: i,
            });
            continue;
        }

        // Single-char punctuation
        let start_line = line;
        let kind = match b {
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b'.' => TokenKind::Dot,
            b',' => TokenKind::Comma,
            b';' => TokenKind::Semicolon,
            b':' => TokenKind::Colon,
            b'<' => TokenKind::LAngle,
            b'>' => TokenKind::RAngle,
            _ => TokenKind::Other,
        };

        let ch = src[i..].chars().next().unwrap();
        i += ch.len_utf8();
        col_utf16 += ch.len_utf16() as u32;

        if !matches!(kind, TokenKind::Other) {
            tokens.push(Token {
                kind,
                line: start_line,
                end_col_utf16: col_utf16,
                byte_end: i,
            });
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_string_interpolation_without_breaking_delimiters() {
        let code = r#"
class Demo {
  void main() {}
  Widget build(BuildContext context) {
    final msg = 'hello ${fn('x')}';
    return Scaffold(
      body: Column(
        children: [
          Text('url: https://example.com'),
        ],
      ),
    );
  }
}
"#;
        let info = parse_dart_syntax(code, false);
        // `main()` inside class `Demo` MUST NOT be detected as top-level main()
        assert_eq!(info.main_line, None);

        let hints = build_closing_label_inlay_hints(&info.closing_labels, 2);
        let labels: Vec<String> = hints
            .iter()
            .map(|h| match &h.label {
                InlayHintLabel::String(s) => s.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(labels.contains(&" // Scaffold".to_string()));
        assert!(labels.contains(&" // Column".to_string()));
    }

    #[test]
    fn skips_constructor_initializers_and_arrow_definitions() {
        let code = r#"
class MyWidget extends StatelessWidget {
  const MyWidget({
    super.key,
    required this.title,
  }) : super();

  Widget Helper(
    int a,
    int b,
  ) => Text('$a $b');
}
"#;
        let info = parse_dart_syntax(code, false);
        let hints = build_closing_label_inlay_hints(&info.closing_labels, 2);
        assert!(hints.is_empty());
    }

    #[test]
    fn parses_nested_groups_and_multiline_tests() {
        let code = r#"
void main() {
  group('Auth', () {
    testWidgets(
      'logs in with ${'nested'}',
      (tester) async {},
    );
  });
}
"#;
        let info = parse_dart_syntax(code, true);
        assert_eq!(info.main_line, Some(1));
        assert_eq!(info.tests.len(), 2);
        assert_eq!(info.tests[0].full_name, "Auth");
    }
}
