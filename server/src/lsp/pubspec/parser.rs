use tower_lsp_server::ls_types::{Position, Range};

use crate::lsp::pubspec::version::{VersionConstraint, strip_quotes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencySection {
    Dependencies,
    DevDependencies,
    DependencyOverrides,
}

impl DependencySection {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dependencies => "dependencies",
            Self::DevDependencies => "dev_dependencies",
            Self::DependencyOverrides => "dependency_overrides",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    /// Hosted on pub.dev (when `url` is `None`) or a custom pub repository.
    Hosted { url: Option<String> },
    /// Shipped with an SDK (e.g. `sdk: flutter`).
    Sdk(String),
    /// Local path dependency with the exact UTF-16 range of the path value.
    Path { path: String, range: Range },
    /// Git repository dependency.
    Git { url: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDependency {
    pub name: String,
    pub section: DependencySection,
    pub source: DependencySource,
    /// Exact raw version token as written in the YAML file (including quotes if present).
    pub raw_constraint: Option<String>,
    pub constraint: VersionConstraint,
    /// Exact UTF-16 LSP range of the package name.
    pub name_range: Range,
    /// Exact UTF-16 LSP range of the version value (falls back to `name_range` when omitted).
    pub version_range: Range,
    /// Position at the end of the dependency's primary line (before trailing comments), for inlay hints.
    pub hint_position: Position,
    /// True if this dependency is a multi-line block map (`pkg:\n  ...`).
    pub is_block_map: bool,
    /// True if this dependency uses an inline YAML flow map (`pkg: { ... }`).
    pub is_flow_map: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlutterAssetEntry {
    pub path: String,
    pub range: Range,
    pub is_font: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionHeader {
    pub name: String,
    pub range: Range,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedPubspec {
    pub project_name: Option<String>,
    pub project_version: Option<String>,
    pub project_description: Option<String>,
    pub is_flutter_project: bool,
    pub is_valid_dart_project: bool,
    pub environment_sdk: Option<VersionConstraint>,
    pub environment_sdk_raw: Option<String>,
    pub environment_flutter_raw: Option<String>,
    pub dependencies: Vec<ParsedDependency>,
    pub assets: Vec<FlutterAssetEntry>,
    pub section_headers: Vec<SectionHeader>,
}

impl ParsedPubspec {
    /// Deterministic fingerprint of dependency & SDK constraint declarations.
    /// Comment, description, or asset edits leave this fingerprint unchanged so
    /// `autoPubGetOnSave` only triggers on real dependency or SDK constraint edits.
    #[must_use]
    pub fn dependency_fingerprint(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.dependencies.len() + 2);
        if let Some(sdk) = &self.environment_sdk_raw {
            out.push(format!("env.sdk={sdk}"));
        }
        if let Some(fl) = &self.environment_flutter_raw {
            out.push(format!("env.flutter={fl}"));
        }
        for dep in &self.dependencies {
            let src = match &dep.source {
                DependencySource::Hosted { url } => {
                    format!("hosted:{}", url.as_deref().unwrap_or(""))
                }
                DependencySource::Sdk(s) => format!("sdk:{s}"),
                DependencySource::Path { path, .. } => format!("path:{path}"),
                DependencySource::Git { url } => format!("git:{}", url.as_deref().unwrap_or("")),
            };
            out.push(format!(
                "{}:{}:{}:{}",
                dep.section.as_str(),
                dep.name,
                src,
                dep.raw_constraint.as_deref().unwrap_or("")
            ));
        }
        out
    }
}

/// Cursor context when requesting version completion inside `pubspec.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionCompletionContext {
    pub package_name: String,
    pub hosted_url: Option<String>,
    pub replace_range: Range,
    pub quote: Option<char>,
    pub use_caret: bool,
    pub filter_prefix: String,
}

/// Unified, tolerant YAML parser for `pubspec.yaml` and `pubspec_overrides.yaml`.
/// Extracts dependencies (single-line, flow-map, multi-line block), `environment`,
/// and `flutter.assets` / `flutter.fonts` with exact UTF-16 LSP spans.
#[must_use]
pub fn parse_pubspec(content: &str) -> ParsedPubspec {
    let lines: Vec<&str> = content.lines().collect();
    let mut result = ParsedPubspec::default();

    let mut has_dart_marker = false;
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        let trimmed = strip_comment(line).trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        let indent = leading_spaces(line);
        if indent != 0 {
            i += 1;
            continue;
        }

        let Some((key, val)) = split_key_value(trimmed) else {
            i += 1;
            continue;
        };

        let line_u32 = i as u32;
        match key {
            "name" => {
                let cleaned = strip_quotes(val).trim();
                if !cleaned.is_empty() {
                    result.project_name = Some(cleaned.to_string());
                }
                result.section_headers.push(SectionHeader {
                    name: "name".to_string(),
                    range: line_key_range(line, line_u32, key),
                });
                i += 1;
            }
            "version" => {
                let cleaned = strip_quotes(val).trim();
                if !cleaned.is_empty() {
                    result.project_version = Some(cleaned.to_string());
                }
                i += 1;
            }
            "description" => {
                let cleaned = strip_quotes(val).trim();
                if !cleaned.is_empty() {
                    result.project_description = Some(cleaned.to_string());
                }
                i += 1;
            }
            "flutter" => {
                result.is_flutter_project = true;
                has_dart_marker = true;
                i += 1;
                i = parse_flutter_section(&lines, i, &mut result);
            }
            "environment" => {
                has_dart_marker = true;
                i += 1;
                while i < lines.len() {
                    let sub_line = lines[i].trim_end_matches('\r');
                    let sub_trimmed = strip_comment(sub_line).trim();
                    if sub_trimmed.is_empty() {
                        i += 1;
                        continue;
                    }
                    let sub_indent = leading_spaces(sub_line);
                    if sub_indent == 0 {
                        break;
                    }
                    if let Some((ekey, eval)) = split_key_value(sub_trimmed) {
                        let cleaned = strip_quotes(eval).trim();
                        match ekey {
                            "sdk" if !cleaned.is_empty() => {
                                result.environment_sdk_raw = Some(cleaned.to_string());
                                result.environment_sdk = Some(VersionConstraint::parse(cleaned));
                            }
                            "flutter" if !cleaned.is_empty() => {
                                result.is_flutter_project = true;
                                result.environment_flutter_raw = Some(cleaned.to_string());
                            }
                            _ => {}
                        }
                    }
                    i += 1;
                }
            }
            "dependencies" | "dev_dependencies" | "dependency_overrides" => {
                has_dart_marker = true;
                let section = match key {
                    "dependencies" => DependencySection::Dependencies,
                    "dev_dependencies" => DependencySection::DevDependencies,
                    _ => DependencySection::DependencyOverrides,
                };
                result.section_headers.push(SectionHeader {
                    name: key.to_string(),
                    range: line_key_range(line, line_u32, key),
                });

                i += 1;
                i = parse_dependency_section(&lines, i, section, &mut result);
            }
            _ => {
                i += 1;
            }
        }
    }

    result.is_valid_dart_project = (result.project_name.is_some() && has_dart_marker)
        || result
            .section_headers
            .iter()
            .any(|h| h.name == "dependency_overrides");
    result
}

fn parse_flutter_section(lines: &[&str], mut i: usize, out: &mut ParsedPubspec) -> usize {
    let mut subsection: Option<&str> = None;

    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        let code = strip_comment(line);
        let trimmed = code.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        let indent = leading_spaces(line);
        if indent == 0 {
            break;
        }

        let line_u32 = i as u32;
        if indent == 2 {
            let key = trimmed.split(':').next().map(str::trim).unwrap_or_default();
            subsection = match key {
                "assets" => Some("assets"),
                "fonts" => Some("fonts"),
                _ => None,
            };
        } else if subsection == Some("assets") {
            if let Some(rest) = trimmed.strip_prefix('-') {
                let candidate = rest.trim();
                if !candidate.is_empty() && !candidate.starts_with("path:") {
                    let cleaned = strip_quotes(candidate).trim();
                    if !cleaned.is_empty() {
                        let after_dash = code.find('-').map_or(indent, |p| p + 1);
                        let range = value_range_after(line, line_u32, after_dash, cleaned);
                        out.assets.push(FlutterAssetEntry {
                            path: cleaned.to_string(),
                            range,
                            is_font: false,
                        });
                    }
                } else if let Some((_, pval)) = candidate.split_once(':') {
                    let cleaned = strip_quotes(pval).trim();
                    if !cleaned.is_empty() {
                        let after_colon = code.find(':').map_or(indent, |p| p + 1);
                        let range = value_range_after(line, line_u32, after_colon, cleaned);
                        out.assets.push(FlutterAssetEntry {
                            path: cleaned.to_string(),
                            range,
                            is_font: false,
                        });
                    }
                }
            }
        } else if subsection == Some("fonts") {
            let clean_trimmed = trimmed.trim_start_matches('-').trim();
            if let Some((k, v)) = clean_trimmed.split_once(':')
                && k.trim() == "asset"
            {
                let cleaned = strip_quotes(v).trim();
                if !cleaned.is_empty() {
                    let after_colon = code.find(':').map_or(indent, |p| p + 1);
                    let range = value_range_after(line, line_u32, after_colon, cleaned);
                    out.assets.push(FlutterAssetEntry {
                        path: cleaned.to_string(),
                        range,
                        is_font: true,
                    });
                }
            }
        }

        i += 1;
    }

    i
}

fn parse_dependency_section(
    lines: &[&str],
    mut i: usize,
    section: DependencySection,
    out: &mut ParsedPubspec,
) -> usize {
    let mut entry_indent: Option<usize> = None;

    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        let code = strip_comment(line);
        let trimmed = code.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        let indent = leading_spaces(line);
        if indent == 0 {
            break;
        }

        let expected_indent = *entry_indent.get_or_insert(indent);
        if indent != expected_indent {
            i += 1;
            continue;
        }

        let Some((pkg_name, val_raw)) = split_key_value(trimmed) else {
            i += 1;
            continue;
        };

        if !is_valid_package_name(pkg_name) {
            i += 1;
            continue;
        }

        let line_u32 = i as u32;
        let name_range = line_key_range(line, line_u32, pkg_name);
        let val_trimmed = val_raw.trim();

        if val_trimmed.starts_with('{') && val_trimmed.ends_with('}') {
            let dep = parse_flow_map_dependency(
                line,
                line_u32,
                pkg_name,
                name_range,
                val_trimmed,
                section,
            );
            if matches!(dep.source, DependencySource::Sdk(ref s) if s == "flutter") {
                out.is_flutter_project = true;
            }
            out.dependencies.push(dep);
            i += 1;
            continue;
        }

        if !val_trimmed.is_empty() {
            let version_range = value_range_on_line(line, line_u32, val_trimmed);
            let hint_position = Position::new(line_u32, utf16_len(code.trim_end()));
            out.dependencies.push(ParsedDependency {
                name: pkg_name.to_string(),
                section,
                source: DependencySource::Hosted { url: None },
                raw_constraint: Some(val_trimmed.to_string()),
                constraint: VersionConstraint::parse(val_trimmed),
                name_range,
                version_range,
                hint_position,
                is_block_map: false,
                is_flow_map: false,
            });
            i += 1;
            continue;
        }

        // Empty value after `pkg_name:` -> implicit `any` hosted dependency OR multi-line map block.
        let mut source = DependencySource::Hosted { url: None };
        let mut raw_constraint: Option<String> = None;
        let mut version_range = name_range;
        let mut hint_position = Position::new(line_u32, utf16_len(code.trim_end()));
        let mut hosted_nested_url: Option<String> = None;
        let mut git_nested_url: Option<String> = None;
        let mut is_block_map = false;

        let mut j = i + 1;
        while j < lines.len() {
            let sub_line = lines[j].trim_end_matches('\r');
            let sub_code = strip_comment(sub_line);
            let sub_trimmed = sub_code.trim();
            if sub_trimmed.is_empty() {
                j += 1;
                continue;
            }
            let sub_indent = leading_spaces(sub_line);
            if sub_indent <= expected_indent {
                break;
            }

            is_block_map = true;
            if let Some((sub_key, sub_val)) = split_key_value(sub_trimmed) {
                let sub_val_trimmed = sub_val.trim();
                let sub_line_u32 = j as u32;
                match sub_key {
                    "sdk" => {
                        let sdk_name = strip_quotes(sub_val_trimmed).trim().to_string();
                        if sdk_name == "flutter" {
                            out.is_flutter_project = true;
                        }
                        source = DependencySource::Sdk(sdk_name);
                    }
                    "path" => {
                        let cleaned = strip_quotes(sub_val_trimmed).trim();
                        let after_colon = sub_code.find(':').map_or(sub_indent, |p| p + 1);
                        let path_range = value_range_after(sub_line, sub_line_u32, after_colon, cleaned);
                        source = DependencySource::Path {
                            path: cleaned.to_string(),
                            range: path_range,
                        };
                    }
                    "git" => {
                        let cleaned = strip_quotes(sub_val_trimmed).trim();
                        let url = (!cleaned.is_empty()).then(|| cleaned.to_string());
                        source = DependencySource::Git { url };
                    }
                    "hosted" => {
                        if !sub_val_trimmed.is_empty() {
                            let url = strip_quotes(sub_val_trimmed)
                                .trim()
                                .trim_end_matches('/')
                                .to_string();
                            source = DependencySource::Hosted { url: Some(url) };
                        } else {
                            source = DependencySource::Hosted { url: None };
                        }
                    }
                    "url" if !sub_val_trimmed.is_empty() => {
                        let cleaned = strip_quotes(sub_val_trimmed)
                            .trim()
                            .trim_end_matches('/')
                            .to_string();
                        match &source {
                            DependencySource::Hosted { .. } => {
                                hosted_nested_url = Some(cleaned);
                            }
                            DependencySource::Git { .. } => {
                                git_nested_url = Some(cleaned);
                            }
                            _ => {}
                        }
                    }
                    "version" => {
                        if !sub_val_trimmed.is_empty() {
                            raw_constraint = Some(sub_val_trimmed.to_string());
                            version_range =
                                value_range_on_line(sub_line, sub_line_u32, sub_val_trimmed);
                        } else {
                            let col = utf16_len(sub_code.trim_end());
                            version_range = Range::new(
                                Position::new(sub_line_u32, col),
                                Position::new(sub_line_u32, col),
                            );
                        }
                        hint_position = Position::new(sub_line_u32, utf16_len(sub_code.trim_end()));
                    }
                    _ => {}
                }
            }
            j += 1;
        }

        if let (DependencySource::Hosted { url }, Some(nested_url)) =
            (&mut source, hosted_nested_url)
            && url.is_none()
        {
            *url = Some(nested_url);
        }
        if let (DependencySource::Git { url }, Some(nested_url)) = (&mut source, git_nested_url)
            && url.is_none()
        {
            *url = Some(nested_url);
        }

        let constraint = raw_constraint
            .as_deref()
            .map_or(VersionConstraint::Any, VersionConstraint::parse);

        out.dependencies.push(ParsedDependency {
            name: pkg_name.to_string(),
            section,
            source,
            raw_constraint,
            constraint,
            name_range,
            version_range,
            hint_position,
            is_block_map,
            is_flow_map: false,
        });

        i = j;
    }

    i
}

fn parse_flow_map_dependency(
    line: &str,
    line_u32: u32,
    pkg_name: &str,
    name_range: Range,
    flow_map: &str,
    section: DependencySection,
) -> ParsedDependency {
    let inner = &flow_map[1..flow_map.len() - 1];
    let mut source = DependencySource::Hosted { url: None };
    let mut raw_constraint: Option<String> = None;
    let mut version_range = name_range;

    for pair in inner.split(',') {
        if let Some((k, v)) = split_key_value(pair.trim()) {
            let v_trimmed = v.trim();
            match k {
                "sdk" => {
                    source = DependencySource::Sdk(strip_quotes(v_trimmed).trim().to_string());
                }
                "path" => {
                    let cleaned = strip_quotes(v_trimmed).trim();
                    let after_key = line
                        .find("path:")
                        .map_or(0, |p| p + "path:".len());
                    let range = value_range_after(line, line_u32, after_key, cleaned);
                    source = DependencySource::Path {
                        path: cleaned.to_string(),
                        range,
                    };
                }
                "git" => {
                    let cleaned = strip_quotes(v_trimmed).trim();
                    source = DependencySource::Git {
                        url: (!cleaned.is_empty()).then(|| cleaned.to_string()),
                    };
                }
                "hosted" if !v_trimmed.is_empty() => {
                    source = DependencySource::Hosted {
                        url: Some(
                            strip_quotes(v_trimmed)
                                .trim()
                                .trim_end_matches('/')
                                .to_string(),
                        ),
                    };
                }
                "version" if !v_trimmed.is_empty() => {
                    raw_constraint = Some(v_trimmed.to_string());
                    let after_ver = line
                        .find("version:")
                        .map_or(0, |p| p + "version:".len());
                    version_range = value_range_after(line, line_u32, after_ver, v_trimmed);
                }
                _ => {}
            }
        }
    }

    let constraint = raw_constraint
        .as_deref()
        .map_or(VersionConstraint::Any, VersionConstraint::parse);
    let hint_position = Position::new(line_u32, utf16_len(strip_comment(line).trim_end()));

    ParsedDependency {
        name: pkg_name.to_string(),
        section,
        source,
        raw_constraint,
        constraint,
        name_range,
        version_range,
        hint_position,
        is_block_map: false,
        is_flow_map: true,
    }
}

/// Computes version-completion context when the cursor is on the value side of a hosted
/// dependency declaration in `pubspec.yaml` or `pubspec_overrides.yaml`.
#[must_use]
pub fn completion_context_at(
    content: &str,
    parsed: &ParsedPubspec,
    pos: Position,
) -> Option<VersionCompletionContext> {
    let raw_line = content.lines().nth(pos.line as usize)?;
    let line = raw_line.trim_end_matches('\r');
    let code = strip_comment(line);
    let code_utf16_len = utf16_len(code);
    if pos.character > code_utf16_len {
        // Cursor is inside a trailing `#` comment.
        return None;
    }

    let dep = parsed.dependencies.iter().find(|d| {
        if !matches!(d.source, DependencySource::Hosted { .. }) {
            return false;
        }
        if d.is_block_map {
            d.version_range.start.line == pos.line && pos.line != d.name_range.start.line
        } else {
            d.name_range.start.line == pos.line
        }
    })?;

    let hosted_url = match &dep.source {
        DependencySource::Hosted { url } => url.clone(),
        _ => return None,
    };

    // Determine the byte offset of the `:` belonging to this version value.
    let colon_byte = if dep.is_flow_map {
        let ver_pos = code.find("version:")?;
        ver_pos + "version".len()
    } else {
        code.find(':')?
    };

    let colon_col_utf16 = utf16_len(&code[..colon_byte]);
    if pos.character <= colon_col_utf16 {
        return None;
    }

    let after_colon = &code[colon_byte + 1..];
    // For flow maps, stop before `,` or `}`.
    let value_slice_raw = if dep.is_flow_map {
        let end = after_colon.find([',', '}']).unwrap_or(after_colon.len());
        &after_colon[..end]
    } else {
        after_colon
    };

    let leading_ws_bytes = value_slice_raw.len() - value_slice_raw.trim_start().len();
    let val_start_byte = colon_byte + 1 + leading_ws_bytes;
    let val_trimmed = value_slice_raw.trim();
    let val_end_byte = val_start_byte + val_trimmed.len();

    let val_start_col = utf16_len(&code[..val_start_byte]);
    let val_end_col = utf16_len(&code[..val_end_byte]);

    if dep.is_flow_map && pos.character > val_end_col && !val_trimmed.is_empty() {
        return None;
    }

    let (replace_start_col, replace_end_col) = if val_trimmed.is_empty() {
        (pos.character, pos.character)
    } else {
        (val_start_col.min(pos.character), val_end_col.max(pos.character))
    };

    let cursor_byte = utf16_col_to_byte(code, pos.character);
    let typed_raw = if cursor_byte >= val_start_byte && cursor_byte <= code.len() {
        code[val_start_byte..cursor_byte].trim()
    } else {
        ""
    };

    let quote = if val_trimmed.starts_with('\'') || typed_raw.starts_with('\'') {
        Some('\'')
    } else if val_trimmed.starts_with('"') || typed_raw.starts_with('"') {
        Some('"')
    } else {
        None
    };

    let typed_unquoted = typed_raw
        .trim_start_matches(['\'', '"'])
        .trim_end_matches(['\'', '"'])
        .trim();

    let use_caret = typed_unquoted.is_empty()
        || typed_unquoted.starts_with('^')
        || typed_unquoted.eq_ignore_ascii_case("any");

    let filter_prefix = typed_unquoted
        .trim_start_matches(['^', '>', '<', '='])
        .trim()
        .to_string();

    Some(VersionCompletionContext {
        package_name: dep.name.clone(),
        hosted_url,
        replace_range: Range::new(
            Position::new(pos.line, replace_start_col),
            Position::new(pos.line, replace_end_col),
        ),
        quote,
        use_caret,
        filter_prefix,
    })
}

fn utf16_col_to_byte(s: &str, col: u32) -> usize {
    let mut acc = 0u32;
    for (idx, ch) in s.char_indices() {
        if acc >= col {
            return idx;
        }
        acc += ch.len_utf16() as u32;
    }
    s.len()
}

/// Strips trailing YAML comments (`# ...`) outside of single/double quotes.
fn strip_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_ws = true;

    for (idx, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double && prev_ws => return &line[..idx],
            _ => {}
        }
        prev_ws = ch.is_whitespace();
    }
    line
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|&c| c == ' ').count()
}

fn split_key_value(trimmed: &str) -> Option<(&str, &str)> {
    let colon_idx = trimmed.find(':')?;
    let key = trimmed[..colon_idx].trim();
    if key.is_empty() {
        return None;
    }
    let val = &trimmed[colon_idx + 1..];
    Some((key, val))
}

fn is_valid_package_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn line_key_range(line: &str, line_idx: u32, key: &str) -> Range {
    let byte_start = line.find(key).unwrap_or(0);
    let start_col = utf16_len(&line[..byte_start]);
    let end_col = start_col + utf16_len(key);
    Range::new(
        Position::new(line_idx, start_col),
        Position::new(line_idx, end_col),
    )
}

fn value_range_on_line(line: &str, line_idx: u32, value: &str) -> Range {
    let search_offset = line.find(':').map_or(0, |i| i + 1);
    value_range_after(line, line_idx, search_offset, value)
}

fn value_range_after(line: &str, line_idx: u32, after_byte: usize, needle: &str) -> Range {
    let base = after_byte.min(line.len());
    let byte_start = line[base..]
        .find(needle)
        .map_or(base, |rel| base + rel);
    let start_col = utf16_len(&line[..byte_start]);
    let end_col = start_col + utf16_len(needle);
    Range::new(
        Position::new(line_idx, start_col),
        Position::new(line_idx, end_col),
    )
}

#[must_use]
pub fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_exact_utf16_columns_for_cjk_assets_and_same_name_path_keys() {
        let yaml = r#"
name: demo
environment:
  sdk: ^3.5.0
dependencies:
  path:
    path: path # 本地路径注释
  dio: ^5.4.0
flutter:
  assets:
    - assets/图标/logo.png # 中文注释
  fonts:
    - family: Roboto
      fonts:
        - asset: fonts/Roboto-Regular.ttf
"#;
        let parsed = parse_pubspec(yaml);
        assert!(parsed.is_valid_dart_project);
        assert!(parsed.is_flutter_project);
        assert_eq!(parsed.dependencies.len(), 2);

        let dep = &parsed.dependencies[0];
        assert_eq!(dep.name, "path");
        match &dep.source {
            DependencySource::Path { path, range } => {
                assert_eq!(path, "path");
                assert_eq!(range.start.character, 10);
                assert_eq!(range.end.character, 14);
            }
            other => panic!("expected Path source, got {other:?}"),
        }

        assert_eq!(parsed.assets.len(), 2);
        let asset = &parsed.assets[0];
        assert_eq!(asset.path, "assets/图标/logo.png");
        assert!(!asset.is_font);
        assert_eq!(asset.range.start.character, 6);
        assert_eq!(
            asset.range.end.character,
            6 + "assets/图标/logo.png".encode_utf16().count() as u32
        );

        let font = &parsed.assets[1];
        assert_eq!(font.path, "fonts/Roboto-Regular.ttf");
        assert!(font.is_font);
    }

    #[test]
    fn dependency_fingerprint_ignores_comments_and_assets_but_detects_dep_changes() {
        let base = "name: app\nenvironment:\n  sdk: ^3.5.0\ndependencies:\n  dio: ^5.4.0\nflutter:\n  assets:\n    - assets/a.png\n";
        let comment_and_asset_edit = "name: app\n# updated comment\nenvironment:\n  sdk: ^3.5.0\ndependencies:\n  dio: ^5.4.0 # inline note\nflutter:\n  assets:\n    - assets/a.png\n    - assets/b.png\n";
        let dep_edit = "name: app\nenvironment:\n  sdk: ^3.5.0\ndependencies:\n  dio: ^5.8.0\nflutter:\n  assets:\n    - assets/a.png\n";

        assert_eq!(
            parse_pubspec(base).dependency_fingerprint(),
            parse_pubspec(comment_and_asset_edit).dependency_fingerprint()
        );
        assert_ne!(
            parse_pubspec(base).dependency_fingerprint(),
            parse_pubspec(dep_edit).dependency_fingerprint()
        );
    }

    #[test]
    fn detects_version_completion_context_for_single_line_multiline_and_flow_map() {
        let yaml = "name: app\ndependencies:\n  dio: ^5.\n  intl: \n  custom_pkg:\n    hosted: https://mirror.example.com\n    version: '^0.1'\n";
        let parsed = parse_pubspec(yaml);

        // `  dio: ^5.` -> cursor at end (line 2, col 10)
        let ctx1 = completion_context_at(yaml, &parsed, Position::new(2, 10)).expect("ctx1");
        assert_eq!(ctx1.package_name, "dio");
        assert_eq!(ctx1.filter_prefix, "5.");
        assert!(ctx1.use_caret);
        assert_eq!(ctx1.replace_range.start.character, 7);
        assert_eq!(ctx1.replace_range.end.character, 10);

        // `  intl: ` -> empty value after colon (line 3, col 8)
        let ctx2 = completion_context_at(yaml, &parsed, Position::new(3, 8)).expect("ctx2");
        assert_eq!(ctx2.package_name, "intl");
        assert_eq!(ctx2.filter_prefix, "");
        assert!(ctx2.use_caret);

        // `    version: '^0.1'` -> quoted caret inside multi-line hosted dep (line 6, col 18)
        let ctx3 = completion_context_at(yaml, &parsed, Position::new(6, 18)).expect("ctx3");
        assert_eq!(ctx3.package_name, "custom_pkg");
        assert_eq!(
            ctx3.hosted_url.as_deref(),
            Some("https://mirror.example.com")
        );
        assert_eq!(ctx3.quote, Some('\''));
        assert_eq!(ctx3.filter_prefix, "0.1");
    }
}
