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

/// Parses a Dart source file in a single fast lexer + balanced-delimiter pass (< 1ms).
/// Extracts `main()`, nested `group`/`test` declarations with full paths, and multiline
/// Flutter Widget constructor closing labels.
pub fn parse_dart_syntax(content: &str, scan_tests: bool) -> DartSyntaxInfo {
    let tokens = tokenize_dart(content);
    let lines: Vec<&str> = content.lines().collect();

    // Precompute matching closing delimiter index for every `(`, `{`, `[`.
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
    // Active enclosing `group(...)` stack: `(close_token_index_of_group_call, group_name)`.
    let mut group_stack: Vec<(usize, String)> = Vec::new();

    for i in 0..tokens.len() {
        // Pop any groups that ended before token `i`
        while let Some(&(end_idx, _)) = group_stack.last() {
            if i > end_idx {
                group_stack.pop();
            } else {
                break;
            }
        }

        let TokenKind::Ident(ref name) = tokens[i].kind else {
            continue;
        };

        // 1. Detect top-level `main(`
        if info.main_line.is_none() && name == "main" {
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
                    && matches!(tokens.get(open_idx).map(|t| &t.kind), Some(TokenKind::LParen))
                    && let Some(close_idx) = match_close[open_idx]
                    && let Some(first_str) = extract_first_string_arg(&tokens, open_idx + 1, close_idx)
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
        // Matches `Scaffold(...)`, `ListView.builder(...)`, `SliverChildBuilderDelegate(...)`, `prefix.Card(...)`
        if is_pascal_case(name) && !CONTROL_KEYWORDS.contains(&name.as_str()) {
            // Avoid double-labeling the class part of `Foo.bar(...)` (handle it when at `Foo` directly)
            let prev_is_dot = i > 0 && matches!(tokens[i - 1].kind, TokenKind::Dot);
            if prev_is_dot {
                // Only allow lowercase prefix like `ui.Card(...)` or `material.Scaffold(...)`
                let prefix_is_lower = i >= 2
                    && matches!(&tokens[i - 2].kind, TokenKind::Ident(p) if p.chars().next().is_some_and(|c| c.is_ascii_lowercase()));
                if !prefix_is_lower {
                    continue;
                }
            }

            // Skip declarations like `class Foo`, `extends Foo`, `implements Foo`, `with Foo`, `on Foo`
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

            // Build label: `Widget` or `Widget.named`
            let mut label = name.clone();
            let mut cursor = i + 1;

            // Optional generic arguments `<T>`
            if let Some(after_generics) = skip_optional_type_args(&tokens, cursor) {
                cursor = after_generics;
            }

            // Optional named constructor `.builder`
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
                    // Skip if immediately followed by `{` or `=>` (that's a constructor/function definition, not a call!)
                    let next_kind = tokens.get(close_idx + 1).map(|t| &t.kind);
                    let is_definition = matches!(next_kind, Some(TokenKind::LBrace))
                        || matches!(
                            tokens.get(close_idx + 1).map(|t| &t.kind),
                            Some(TokenKind::Ident(kw)) if matches!(kw.as_str(), "async" | "sync" | "super")
                        );

                    // Skip if the closing line already has a trailing `//` comment
                    let line_already_has_comment = lines
                        .get(close_line as usize)
                        .is_some_and(|l| l.contains("//"));

                    // Also skip if another closing label is already placed on the exact same line
                    // (keep the outermost / last one on that line for visual clarity, or place right after `)`)
                    if !is_definition && !line_already_has_comment {
                        // Advance past immediate trailing `,` or `;` if it's on the same line so the
                        // hint renders cleanly at the end of the closing delimiter (e.g. `), // Scaffold`).
                        let mut hint_col = close_tok.end_col_utf16;
                        if let Some(after) = tokens.get(close_idx + 1)
                            && after.line == close_line
                            && matches!(after.kind, TokenKind::Comma | TokenKind::Semicolon)
                        {
                            hint_col = after.end_col_utf16;
                        }

                        // Ensure we don't emit closing label inside a chained call on the same line
                        let rest_of_line = content
                            .get(close_tok.byte_end..)
                            .and_then(|s| s.lines().next())
                            .unwrap_or_default()
                            .trim();
                        let ends_cleanly = rest_of_line.is_empty()
                            || rest_of_line == ","
                            || rest_of_line == ";"
                            || rest_of_line == "),"
                            || rest_of_line == ");";

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

/// Converts parsed [`ClosingLabelEntry`] items into standard LSP `InlayHint`s filtered by `min_lines`.
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

/// Fast, comment-aware & string-aware Dart tokenizer tracking exact UTF-16 line/column positions.
fn tokenize_dart(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut line = 0u32;
    let mut col_utf16 = 0u32;
    let mut tokens = Vec::with_capacity(src.len() / 6);

    while i < len {
        let b = bytes[i];

        // Whitespace
        if b == b'\n' {
            line += 1;
            col_utf16 = 0;
            i += 1;
            continue;
        }
        if b == b'\r' || b == b' ' || b == b'\t' {
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

        // Raw string `r'...'`, `r"..."`, `r'''...'''`, `r"""..."""`
        let is_raw_str = b == b'r'
            && i + 1 < len
            && (bytes[i + 1] == b'\'' || bytes[i + 1] == b'"');
        if is_raw_str || b == b'\'' || b == b'"' {
            let start_line = line;
            let raw = is_raw_str;
            if raw {
                i += 1;
                col_utf16 += 1;
            }
            let quote = bytes[i];
            let is_triple = i + 2 < len && bytes[i + 1] == quote && bytes[i + 2] == quote;
            if is_triple {
                i += 3;
                col_utf16 += 3;
            } else {
                i += 1;
                col_utf16 += 1;
            }

            let mut value = String::new();
            while i < len {
                if is_triple {
                    if i + 2 < len
                        && bytes[i] == quote
                        && bytes[i + 1] == quote
                        && bytes[i + 2] == quote
                    {
                        i += 3;
                        col_utf16 += 3;
                        break;
                    }
                } else if bytes[i] == quote {
                    i += 1;
                    col_utf16 += 1;
                    break;
                }

                if !raw && bytes[i] == b'\\' && i + 1 < len {
                    let esc = src[i + 1..].chars().next().unwrap();
                    value.push(esc);
                    i += 1 + esc.len_utf8();
                    col_utf16 += 1 + esc.len_utf16() as u32;
                    continue;
                }

                let ch = src[i..].chars().next().unwrap();
                if ch == '\n' {
                    line += 1;
                    col_utf16 = 0;
                    value.push('\n');
                } else {
                    col_utf16 += ch.len_utf16() as u32;
                    value.push(ch);
                }
                i += ch.len_utf8();
            }

            tokens.push(Token {
                kind: TokenKind::StringLit(value),
                line: start_line,
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
    fn parses_nested_groups_multiline_tests_and_bloc_test() {
        let code = r#"
import 'package:flutter_test/flutter_test.dart';

Future<void>
main() async {
  group('AuthBloc', () {
    group('login', () {
      testWidgets(
        'emits authenticated',
        (tester) async {},
      );
      blocTest<AuthBloc, AuthState>(
        'handles timeout',
        build: () => AuthBloc(),
      );
    });
    patrolTest('end to end flow', ($) async {});
  });
}
"#;
        let info = parse_dart_syntax(code, true);
        assert_eq!(info.main_line, Some(4));
        let full_names: Vec<&str> = info.tests.iter().map(|t| t.full_name.as_str()).collect();
        assert_eq!(
            full_names,
            vec![
                "AuthBloc",
                "AuthBloc login",
                "AuthBloc login emits authenticated",
                "AuthBloc login handles timeout",
                "AuthBloc end to end flow",
            ]
        );
    }

    #[test]
    fn generates_closing_labels_for_multiline_widgets_and_skips_comments() {
        let code = r#"
class MyPage extends StatelessWidget {
  Widget build(BuildContext context) {
    return Scaffold(
      body: Column(
        children: [
          ListView.builder(
            itemCount: 10,
            itemBuilder: (ctx, i) => Text('$i'),
          ),
        ],
      ),
    );
  }
}
"#;
        let info = parse_dart_syntax(code, false);
        let hints = build_closing_label_inlay_hints(&info.closing_labels, 2);
        let labels: Vec<String> = hints
            .iter()
            .map(|h| match &h.label {
                InlayHintLabel::String(s) => s.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(labels.contains(&" // ListView.builder".to_string()));
        assert!(labels.contains(&" // Column".to_string()));
        assert!(labels.contains(&" // Scaffold".to_string()));
        assert!(!labels.iter().any(|l| l.contains("Text")));
    }
}
