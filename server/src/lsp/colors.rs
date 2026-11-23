use std::sync::OnceLock;

use regex::Regex;
use tower_lsp_server::ls_types::{
    Color, ColorInformation, ColorPresentation, Position, Range, TextEdit,
};

use crate::lsp::dart_parser::mask_dart_comments_and_strings;

/// Extracts all Flutter color literals and Material / Cupertino color constants from a Dart file
/// for LSP `textDocument/documentColor`.
///
/// Uses `mask_dart_comments_and_strings` so block comments, line comments, and string literals
/// NEVER produce false color swatches, while supporting multiline `Color.from(...)` (any arg
/// order), multiline `fromARGB`/`fromRGBO`, Dart digit underscores (`0xFF_21_96_F3`), and hex
/// arguments.
pub fn find_dart_colors(content: &str) -> Vec<ColorInformation> {
    static HEX_RE: OnceLock<Regex> = OnceLock::new();
    static ARGB_RE: OnceLock<Regex> = OnceLock::new();
    static RGBO_RE: OnceLock<Regex> = OnceLock::new();
    static FROM_RE: OnceLock<Regex> = OnceLock::new();
    static MATERIAL_RE: OnceLock<Regex> = OnceLock::new();
    static CUPERTINO_RE: OnceLock<Regex> = OnceLock::new();

    let hex_re = HEX_RE.get_or_init(|| {
        Regex::new(r"\bColor\s*\(\s*0[xX]([0-9a-fA-F_]{8,15})\s*\)").expect("valid hex regex")
    });
    let argb_re = ARGB_RE.get_or_init(|| {
        Regex::new(
            r"\bColor\.fromARGB\s*\(\s*(0[xX][0-9a-fA-F_]+|[0-9_]+)\s*,\s*(0[xX][0-9a-fA-F_]+|[0-9_]+)\s*,\s*(0[xX][0-9a-fA-F_]+|[0-9_]+)\s*,\s*(0[xX][0-9a-fA-F_]+|[0-9_]+)\s*,?\s*\)",
        )
        .expect("valid argb regex")
    });
    let rgbo_re = RGBO_RE.get_or_init(|| {
        Regex::new(
            r"\bColor\.fromRGBO\s*\(\s*(0[xX][0-9a-fA-F_]+|[0-9_]+)\s*,\s*(0[xX][0-9a-fA-F_]+|[0-9_]+)\s*,\s*(0[xX][0-9a-fA-F_]+|[0-9_]+)\s*,\s*([0-9_]*\.?[0-9_]+)\s*,?\s*\)",
        )
        .expect("valid rgbo regex")
    });
    let from_re = FROM_RE.get_or_init(|| {
        Regex::new(r"\bColor\.from\s*\(([^()]{1,200})\)").expect("valid Color.from regex")
    });
    let material_re = MATERIAL_RE.get_or_init(|| {
        Regex::new(r"\bColors\.([a-zA-Z0-9]+)(?:\[(\d+)\]|\.shade(\d+))?")
            .expect("valid material regex")
    });
    let cupertino_re = CUPERTINO_RE.get_or_init(|| {
        Regex::new(r"\bCupertinoColors\.([a-zA-Z0-9]+)").expect("valid cupertino regex")
    });

    let masked = mask_dart_comments_and_strings(content);
    let line_starts = compute_line_starts(content);
    let mut results = Vec::new();

    // 1. Color(0xAARRGGBB) with optional Dart digit underscores
    for cap in hex_re.captures_iter(&masked) {
        if let (Some(full), Some(hex_raw)) = (cap.get(0), cap.get(1)) {
            let cleaned: String = hex_raw.as_str().chars().filter(|&c| c != '_').collect();
            if cleaned.len() == 8
                && let Ok(argb) = u32::from_str_radix(&cleaned, 16)
            {
                results.push(ColorInformation {
                    range: offset_span_to_range(content, &line_starts, full.start(), full.end()),
                    color: argb_to_lsp_color(argb),
                });
            }
        }
    }

    // 2. Color.fromARGB(a, r, g, b) (decimal or hex, single or multiline)
    for cap in argb_re.captures_iter(&masked) {
        if let (Some(full), Some(a), Some(r), Some(g), Some(b)) =
            (cap.get(0), cap.get(1), cap.get(2), cap.get(3), cap.get(4))
            && let (Some(a), Some(r), Some(g), Some(b)) = (
                parse_dart_u8(a.as_str()),
                parse_dart_u8(r.as_str()),
                parse_dart_u8(g.as_str()),
                parse_dart_u8(b.as_str()),
            )
        {
            results.push(ColorInformation {
                range: offset_span_to_range(content, &line_starts, full.start(), full.end()),
                color: rgba_u8_to_lsp_color(r, g, b, a),
            });
        }
    }

    // 3. Color.fromRGBO(r, g, b, opacity) (single or multiline)
    for cap in rgbo_re.captures_iter(&masked) {
        if let (Some(full), Some(r), Some(g), Some(b), Some(o)) =
            (cap.get(0), cap.get(1), cap.get(2), cap.get(3), cap.get(4))
            && let (Some(r), Some(g), Some(b), Some(o)) = (
                parse_dart_u8(r.as_str()),
                parse_dart_u8(g.as_str()),
                parse_dart_u8(b.as_str()),
                parse_dart_f32(o.as_str()),
            )
        {
            results.push(ColorInformation {
                range: offset_span_to_range(content, &line_starts, full.start(), full.end()),
                color: Color {
                    red: f32::from(r) / 255.0,
                    green: f32::from(g) / 255.0,
                    blue: f32::from(b) / 255.0,
                    alpha: o.clamp(0.0, 1.0),
                },
            });
        }
    }

    // 4. Color.from(alpha: ..., red: ..., green: ..., blue: ...) in ANY order, single or multiline
    for cap in from_re.captures_iter(&masked) {
        if let (Some(full), Some(body)) = (cap.get(0), cap.get(1))
            && let Some(color) = parse_color_from_named_args(body.as_str())
        {
            results.push(ColorInformation {
                range: offset_span_to_range(content, &line_starts, full.start(), full.end()),
                color,
            });
        }
    }

    // 5. Material Colors.<name>([shade] | .shade<N>)
    for cap in material_re.captures_iter(&masked) {
        if let (Some(full), Some(name)) = (cap.get(0), cap.get(1)) {
            let shade = cap
                .get(2)
                .or_else(|| cap.get(3))
                .and_then(|m| m.as_str().parse::<u16>().ok());
            if let Some(argb) = lookup_material_color(name.as_str(), shade) {
                results.push(ColorInformation {
                    range: offset_span_to_range(content, &line_starts, full.start(), full.end()),
                    color: argb_to_lsp_color(argb),
                });
            }
        }
    }

    // 6. CupertinoColors.<name>
    for cap in cupertino_re.captures_iter(&masked) {
        if let (Some(full), Some(name)) = (cap.get(0), cap.get(1))
            && let Some(argb) = lookup_cupertino_color(name.as_str())
        {
            results.push(ColorInformation {
                range: offset_span_to_range(content, &line_starts, full.start(), full.end()),
                color: argb_to_lsp_color(argb),
            });
        }
    }

    results
}

fn parse_color_from_named_args(body: &str) -> Option<Color> {
    let mut alpha: Option<f32> = None;
    let mut red: Option<f32> = None;
    let mut green: Option<f32> = None;
    let mut blue: Option<f32> = None;

    for part in body.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((key, val)) = trimmed.split_once(':') else {
            continue;
        };
        let k = key.trim();
        let v = val.trim();
        match k {
            "alpha" => alpha = parse_dart_f32(v),
            "red" => red = parse_dart_f32(v),
            "green" => green = parse_dart_f32(v),
            "blue" => blue = parse_dart_f32(v),
            _ => {}
        }
    }

    Some(Color {
        alpha: alpha?.clamp(0.0, 1.0),
        red: red?.clamp(0.0, 1.0),
        green: green?.clamp(0.0, 1.0),
        blue: blue?.clamp(0.0, 1.0),
    })
}

fn parse_dart_u8(raw: &str) -> Option<u8> {
    let cleaned: String = raw.trim().chars().filter(|&c| c != '_').collect();
    if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        u8::from_str_radix(hex, 16).ok()
    } else {
        cleaned.parse::<u8>().ok()
    }
}

fn parse_dart_f32(raw: &str) -> Option<f32> {
    let cleaned: String = raw.trim().chars().filter(|&c| c != '_').collect();
    cleaned.parse::<f32>().ok()
}

fn compute_line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (idx, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

fn offset_to_position(src: &str, line_starts: &[usize], offset: usize) -> Position {
    let line_idx = match line_starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let line_start = line_starts[line_idx];
    let slice = &src[line_start..offset.min(src.len())];
    let col_utf16 = slice
        .chars()
        .filter(|&c| c != '\r')
        .map(|c| c.len_utf16() as u32)
        .sum();
    Position::new(line_idx as u32, col_utf16)
}

fn offset_span_to_range(src: &str, line_starts: &[usize], start: usize, end: usize) -> Range {
    Range {
        start: offset_to_position(src, line_starts, start),
        end: offset_to_position(src, line_starts, end),
    }
}

/// Builds canonical Dart `Color(0xAARRGGBB)` and `Color.fromARGB(...)` presentations.
pub fn build_color_presentations(color: Color, range: Range) -> Vec<ColorPresentation> {
    let a = (color.alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    let r = (color.red.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (color.green.clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (color.blue.clamp(0.0, 1.0) * 255.0).round() as u8;

    let hex_label = format!("Color(0x{a:02X}{r:02X}{g:02X}{b:02X})");
    let argb_label = format!("Color.fromARGB({a}, {r}, {g}, {b})");

    vec![
        ColorPresentation {
            label: hex_label.clone(),
            text_edit: Some(TextEdit {
                range,
                new_text: hex_label,
            }),
            additional_text_edits: None,
        },
        ColorPresentation {
            label: argb_label.clone(),
            text_edit: Some(TextEdit {
                range,
                new_text: argb_label,
            }),
            additional_text_edits: None,
        },
    ]
}

fn argb_to_lsp_color(argb: u32) -> Color {
    let a = ((argb >> 24) & 0xff) as u8;
    let r = ((argb >> 16) & 0xff) as u8;
    let g = ((argb >> 8) & 0xff) as u8;
    let b = (argb & 0xff) as u8;
    rgba_u8_to_lsp_color(r, g, b, a)
}

fn rgba_u8_to_lsp_color(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color {
        red: f32::from(r) / 255.0,
        green: f32::from(g) / 255.0,
        blue: f32::from(b) / 255.0,
        alpha: f32::from(a) / 255.0,
    }
}

/// Complete Flutter Material primary (18 palettes, shades 50–900) and accent (16 palettes,
/// shades 100/200/400/700) color constant lookup.
pub fn lookup_material_color(name: &str, shade: Option<u16>) -> Option<u32> {
    if shade.is_none() {
        let direct = match name {
            "transparent" => Some(0x0000_0000),
            "black" => Some(0xFF00_0000),
            "black87" => Some(0xDD00_0000),
            "black54" => Some(0x8A00_0000),
            "black45" => Some(0x7300_0000),
            "black38" => Some(0x6100_0000),
            "black26" => Some(0x4200_0000),
            "black12" => Some(0x1F00_0000),
            "white" => Some(0xFFFF_FFFF),
            "white70" => Some(0xB3FF_FFFF),
            "white60" => Some(0x99FF_FFFF),
            "white54" => Some(0x8AFF_FFFF),
            "white38" => Some(0x62FF_FFFF),
            "white30" => Some(0x4DFF_FFFF),
            "white24" => Some(0x3DFF_FFFF),
            "white12" => Some(0x1FFF_FFFF),
            "white10" => Some(0x1AFF_FFFF),
            _ => None,
        };
        if direct.is_some() {
            return direct;
        }
    }

    let is_accent = name.ends_with("Accent");
    let s = shade.unwrap_or(if is_accent { 200 } else { 500 });

    match (name, s) {
        // Red
        ("red", 50) => Some(0xFFFFEBEE),
        ("red", 100) => Some(0xFFFFCDD2),
        ("red", 200) => Some(0xFFEF9A9A),
        ("red", 300) => Some(0xFFE57373),
        ("red", 400) => Some(0xFFEF5350),
        ("red", 500) => Some(0xFFF44336),
        ("red", 600) => Some(0xFFE53935),
        ("red", 700) => Some(0xFFD32F2F),
        ("red", 800) => Some(0xFFC62828),
        ("red", 900) => Some(0xFFB71C1C),
        ("redAccent", 100) => Some(0xFFFF8A80),
        ("redAccent", 200) => Some(0xFFFF5252),
        ("redAccent", 400) => Some(0xFFFF1744),
        ("redAccent", 700) => Some(0xFFD50000),

        // Pink
        ("pink", 50) => Some(0xFFFCE4EC),
        ("pink", 100) => Some(0xFFF8BBD0),
        ("pink", 200) => Some(0xFFF48FB1),
        ("pink", 300) => Some(0xFFF06292),
        ("pink", 400) => Some(0xFFEC407A),
        ("pink", 500) => Some(0xFFE91E63),
        ("pink", 600) => Some(0xFFD81B60),
        ("pink", 700) => Some(0xFFC2185B),
        ("pink", 800) => Some(0xFFAD1457),
        ("pink", 900) => Some(0xFF880E4F),
        ("pinkAccent", 100) => Some(0xFFFF80AB),
        ("pinkAccent", 200) => Some(0xFFFF4081),
        ("pinkAccent", 400) => Some(0xFFF50057),
        ("pinkAccent", 700) => Some(0xFFC51162),

        // Purple
        ("purple", 50) => Some(0xFFF3E5F5),
        ("purple", 100) => Some(0xFFE1BEE7),
        ("purple", 200) => Some(0xFFCE93D8),
        ("purple", 300) => Some(0xFFBA68C8),
        ("purple", 400) => Some(0xFFAB47BC),
        ("purple", 500) => Some(0xFF9C27B0),
        ("purple", 600) => Some(0xFF8E24AA),
        ("purple", 700) => Some(0xFF7B1FA2),
        ("purple", 800) => Some(0xFF6A1B9A),
        ("purple", 900) => Some(0xFF4A148C),
        ("purpleAccent", 100) => Some(0xFFEA80FC),
        ("purpleAccent", 200) => Some(0xFFE040FB),
        ("purpleAccent", 400) => Some(0xFFD500F9),
        ("purpleAccent", 700) => Some(0xFFAA00FF),

        // Deep Purple
        ("deepPurple", 50) => Some(0xFFEDE7F6),
        ("deepPurple", 100) => Some(0xFFD1C4E9),
        ("deepPurple", 200) => Some(0xFFB39DDB),
        ("deepPurple", 300) => Some(0xFF9575CD),
        ("deepPurple", 400) => Some(0xFF7E57C2),
        ("deepPurple", 500) => Some(0xFF673AB7),
        ("deepPurple", 600) => Some(0xFF5E35B1),
        ("deepPurple", 700) => Some(0xFF512DA8),
        ("deepPurple", 800) => Some(0xFF4527A0),
        ("deepPurple", 900) => Some(0xFF311B92),
        ("deepPurpleAccent", 100) => Some(0xFFB388FF),
        ("deepPurpleAccent", 200) => Some(0xFF7C4DFF),
        ("deepPurpleAccent", 400) => Some(0xFF651FFF),
        ("deepPurpleAccent", 700) => Some(0xFF6200EA),

        // Indigo
        ("indigo", 50) => Some(0xFFE8EAF6),
        ("indigo", 100) => Some(0xFFC5CAE9),
        ("indigo", 200) => Some(0xFF9FA8DA),
        ("indigo", 300) => Some(0xFF7986CB),
        ("indigo", 400) => Some(0xFF5C6BC0),
        ("indigo", 500) => Some(0xFF3F51B5),
        ("indigo", 600) => Some(0xFF3949AB),
        ("indigo", 700) => Some(0xFF303F9F),
        ("indigo", 800) => Some(0xFF283593),
        ("indigo", 900) => Some(0xFF1A237E),
        ("indigoAccent", 100) => Some(0xFF8C9EFF),
        ("indigoAccent", 200) => Some(0xFF536DFE),
        ("indigoAccent", 400) => Some(0xFF3D5AFE),
        ("indigoAccent", 700) => Some(0xFF304FFE),

        // Blue
        ("blue", 50) => Some(0xFFE3F2FD),
        ("blue", 100) => Some(0xFFBBDEFB),
        ("blue", 200) => Some(0xFF90CAF9),
        ("blue", 300) => Some(0xFF64B5F6),
        ("blue", 400) => Some(0xFF42A5F5),
        ("blue", 500) => Some(0xFF2196F3),
        ("blue", 600) => Some(0xFF1E88E5),
        ("blue", 700) => Some(0xFF1976D2),
        ("blue", 800) => Some(0xFF1565C0),
        ("blue", 900) => Some(0xFF0D47A1),
        ("blueAccent", 100) => Some(0xFF82B1FF),
        ("blueAccent", 200) => Some(0xFF448AFF),
        ("blueAccent", 400) => Some(0xFF2979FF),
        ("blueAccent", 700) => Some(0xFF2962FF),

        // Light Blue
        ("lightBlue", 50) => Some(0xFFE1F5FE),
        ("lightBlue", 100) => Some(0xFFB3E5FC),
        ("lightBlue", 200) => Some(0xFF81D4FA),
        ("lightBlue", 300) => Some(0xFF4FC3F7),
        ("lightBlue", 400) => Some(0xFF29B6F6),
        ("lightBlue", 500) => Some(0xFF03A9F4),
        ("lightBlue", 600) => Some(0xFF039BE5),
        ("lightBlue", 700) => Some(0xFF0288D1),
        ("lightBlue", 800) => Some(0xFF0277BD),
        ("lightBlue", 900) => Some(0xFF01579B),
        ("lightBlueAccent", 100) => Some(0xFF80D8FF),
        ("lightBlueAccent", 200) => Some(0xFF40C4FF),
        ("lightBlueAccent", 400) => Some(0xFF00B0FF),
        ("lightBlueAccent", 700) => Some(0xFF0091EA),

        // Cyan
        ("cyan", 50) => Some(0xFFE0F7FA),
        ("cyan", 100) => Some(0xFFB2EBF2),
        ("cyan", 200) => Some(0xFF80DEEA),
        ("cyan", 300) => Some(0xFF4DD0E1),
        ("cyan", 400) => Some(0xFF26C6DA),
        ("cyan", 500) => Some(0xFF00BCD4),
        ("cyan", 600) => Some(0xFF00ACC1),
        ("cyan", 700) => Some(0xFF0097A7),
        ("cyan", 800) => Some(0xFF00838F),
        ("cyan", 900) => Some(0xFF006064),
        ("cyanAccent", 100) => Some(0xFF84FFFF),
        ("cyanAccent", 200) => Some(0xFF18FFFF),
        ("cyanAccent", 400) => Some(0xFF00E5FF),
        ("cyanAccent", 700) => Some(0xFF00B8D4),

        // Teal
        ("teal", 50) => Some(0xFFE0F2F1),
        ("teal", 100) => Some(0xFFB2DFDB),
        ("teal", 200) => Some(0xFF80CBC4),
        ("teal", 300) => Some(0xFF4DB6AC),
        ("teal", 400) => Some(0xFF26A69A),
        ("teal", 500) => Some(0xFF009688),
        ("teal", 600) => Some(0xFF00897B),
        ("teal", 700) => Some(0xFF00796B),
        ("teal", 800) => Some(0xFF00695C),
        ("teal", 900) => Some(0xFF004D40),
        ("tealAccent", 100) => Some(0xFFA7FFEB),
        ("tealAccent", 200) => Some(0xFF64FFDA),
        ("tealAccent", 400) => Some(0xFF1DE9B6),
        ("tealAccent", 700) => Some(0xFF00BFA5),

        // Green
        ("green", 50) => Some(0xFFE8F5E9),
        ("green", 100) => Some(0xFFC8E6C9),
        ("green", 200) => Some(0xFFA5D6A7),
        ("green", 300) => Some(0xFF81C784),
        ("green", 400) => Some(0xFF66BB6A),
        ("green", 500) => Some(0xFF4CAF50),
        ("green", 600) => Some(0xFF43A047),
        ("green", 700) => Some(0xFF388E3C),
        ("green", 800) => Some(0xFF2E7D32),
        ("green", 900) => Some(0xFF1B5E20),
        ("greenAccent", 100) => Some(0xFFB9F6CA),
        ("greenAccent", 200) => Some(0xFF69F0AE),
        ("greenAccent", 400) => Some(0xFF00E676),
        ("greenAccent", 700) => Some(0xFF00C853),

        // Light Green
        ("lightGreen", 50) => Some(0xFFF1F8E9),
        ("lightGreen", 100) => Some(0xFFDCEDC8),
        ("lightGreen", 200) => Some(0xFFC5E1A5),
        ("lightGreen", 300) => Some(0xFFAED581),
        ("lightGreen", 400) => Some(0xFF9CCC65),
        ("lightGreen", 500) => Some(0xFF8BC34A),
        ("lightGreen", 600) => Some(0xFF7CB342),
        ("lightGreen", 700) => Some(0xFF689F38),
        ("lightGreen", 800) => Some(0xFF558B2F),
        ("lightGreen", 900) => Some(0xFF33691E),
        ("lightGreenAccent", 100) => Some(0xFFCCFF90),
        ("lightGreenAccent", 200) => Some(0xFFB2FF59),
        ("lightGreenAccent", 400) => Some(0xFF76FF03),
        ("lightGreenAccent", 700) => Some(0xFF64DD17),

        // Lime
        ("lime", 50) => Some(0xFFF9FBE7),
        ("lime", 100) => Some(0xFFF0F4C3),
        ("lime", 200) => Some(0xFFE6EE9C),
        ("lime", 300) => Some(0xFFDCE775),
        ("lime", 400) => Some(0xFFD4E157),
        ("lime", 500) => Some(0xFFCDDC39),
        ("lime", 600) => Some(0xFFC0CA33),
        ("lime", 700) => Some(0xFFAFB42B),
        ("lime", 800) => Some(0xFF9E9D24),
        ("lime", 900) => Some(0xFF827717),
        ("limeAccent", 100) => Some(0xFFF4FF81),
        ("limeAccent", 200) => Some(0xFFEEFF41),
        ("limeAccent", 400) => Some(0xFFC6FF00),
        ("limeAccent", 700) => Some(0xFFAEEA00),

        // Yellow
        ("yellow", 50) => Some(0xFFFFFDE7),
        ("yellow", 100) => Some(0xFFFFF9C4),
        ("yellow", 200) => Some(0xFFFFF59D),
        ("yellow", 300) => Some(0xFFFFF176),
        ("yellow", 400) => Some(0xFFFFEE58),
        ("yellow", 500) => Some(0xFFFFEB3B),
        ("yellow", 600) => Some(0xFFFDD835),
        ("yellow", 700) => Some(0xFFFBC02D),
        ("yellow", 800) => Some(0xFFF9A825),
        ("yellow", 900) => Some(0xFFF57F17),
        ("yellowAccent", 100) => Some(0xFFFFFF8D),
        ("yellowAccent", 200) => Some(0xFFFFFF00),
        ("yellowAccent", 400) => Some(0xFFFFEA00),
        ("yellowAccent", 700) => Some(0xFFFFD600),

        // Amber
        ("amber", 50) => Some(0xFFFFF8E1),
        ("amber", 100) => Some(0xFFFFECB3),
        ("amber", 200) => Some(0xFFFFE082),
        ("amber", 300) => Some(0xFFFFD54F),
        ("amber", 400) => Some(0xFFFFCA28),
        ("amber", 500) => Some(0xFFFFC107),
        ("amber", 600) => Some(0xFFFFB300),
        ("amber", 700) => Some(0xFFFFA000),
        ("amber", 800) => Some(0xFFFF8F00),
        ("amber", 900) => Some(0xFFFF6F00),
        ("amberAccent", 100) => Some(0xFFFFE57F),
        ("amberAccent", 200) => Some(0xFFFFD740),
        ("amberAccent", 400) => Some(0xFFFFC400),
        ("amberAccent", 700) => Some(0xFFFFAB00),

        // Orange
        ("orange", 50) => Some(0xFFFFF3E0),
        ("orange", 100) => Some(0xFFFFE0B2),
        ("orange", 200) => Some(0xFFFFCC80),
        ("orange", 300) => Some(0xFFFFB74D),
        ("orange", 400) => Some(0xFFFFA726),
        ("orange", 500) => Some(0xFFFF9800),
        ("orange", 600) => Some(0xFFFB8C00),
        ("orange", 700) => Some(0xFFF57C00),
        ("orange", 800) => Some(0xFFEF6C00),
        ("orange", 900) => Some(0xFFE65100),
        ("orangeAccent", 100) => Some(0xFFFFD180),
        ("orangeAccent", 200) => Some(0xFFFFAB40),
        ("orangeAccent", 400) => Some(0xFFFF9100),
        ("orangeAccent", 700) => Some(0xFFFF6D00),

        // Deep Orange
        ("deepOrange", 50) => Some(0xFFFBE9E7),
        ("deepOrange", 100) => Some(0xFFFFCCBC),
        ("deepOrange", 200) => Some(0xFFFFAB91),
        ("deepOrange", 300) => Some(0xFFFF8A65),
        ("deepOrange", 400) => Some(0xFFFF7043),
        ("deepOrange", 500) => Some(0xFFFF5722),
        ("deepOrange", 600) => Some(0xFFF4511E),
        ("deepOrange", 700) => Some(0xFFE64A19),
        ("deepOrange", 800) => Some(0xFFD84315),
        ("deepOrange", 900) => Some(0xFFBF360C),
        ("deepOrangeAccent", 100) => Some(0xFFFF9E80),
        ("deepOrangeAccent", 200) => Some(0xFFFF6E40),
        ("deepOrangeAccent", 400) => Some(0xFFFF3D00),
        ("deepOrangeAccent", 700) => Some(0xFFDD2C00),

        // Brown
        ("brown", 50) => Some(0xFFEFEBE9),
        ("brown", 100) => Some(0xFFD7CCC8),
        ("brown", 200) => Some(0xFFBCAAA4),
        ("brown", 300) => Some(0xFFA1887F),
        ("brown", 400) => Some(0xFF8D6E63),
        ("brown", 500) => Some(0xFF795548),
        ("brown", 600) => Some(0xFF6D4C41),
        ("brown", 700) => Some(0xFF5D4037),
        ("brown", 800) => Some(0xFF4E342E),
        ("brown", 900) => Some(0xFF3E2723),

        // Grey
        ("grey", 50) => Some(0xFFFAFAFA),
        ("grey", 100) => Some(0xFFF5F5F5),
        ("grey", 200) => Some(0xFFEEEEEE),
        ("grey", 300) => Some(0xFFE0E0E0),
        ("grey", 350) => Some(0xFFD6D6D6),
        ("grey", 400) => Some(0xFFBDBDBD),
        ("grey", 500) => Some(0xFF9E9E9E),
        ("grey", 600) => Some(0xFF757575),
        ("grey", 700) => Some(0xFF616161),
        ("grey", 800) => Some(0xFF424242),
        ("grey", 850) => Some(0xFF303030),
        ("grey", 900) => Some(0xFF212121),

        // Blue Grey
        ("blueGrey", 50) => Some(0xFFECEFF1),
        ("blueGrey", 100) => Some(0xFFCFD8DC),
        ("blueGrey", 200) => Some(0xFFB0BEC5),
        ("blueGrey", 300) => Some(0xFF90A4AE),
        ("blueGrey", 400) => Some(0xFF78909C),
        ("blueGrey", 500) => Some(0xFF607D8B),
        ("blueGrey", 600) => Some(0xFF546E7A),
        ("blueGrey", 700) => Some(0xFF455A64),
        ("blueGrey", 800) => Some(0xFF37474F),
        ("blueGrey", 900) => Some(0xFF263238),

        _ => None,
    }
}

pub fn lookup_cupertino_color(name: &str) -> Option<u32> {
    match name {
        "activeBlue" | "systemBlue" => Some(0xFF007AFF),
        "activeGreen" | "systemGreen" => Some(0xFF34C759),
        "activeOrange" | "systemOrange" => Some(0xFFFF9500),
        "destructiveRed" | "systemRed" => Some(0xFFFF3B30),
        "systemYellow" => Some(0xFFFFCC00),
        "systemPink" => Some(0xFFFF2D55),
        "systemPurple" => Some(0xFFAF52DE),
        "systemTeal" => Some(0xFF5AC8FA),
        "systemIndigo" => Some(0xFF5856D6),
        "systemMint" => Some(0xFF00C7BE),
        "systemCyan" => Some(0xFF32ADE6),
        "systemBrown" => Some(0xFFA2845E),
        "systemGrey" => Some(0xFF8E8E93),
        "systemGrey2" => Some(0xFFAEAEB2),
        "systemGrey3" => Some(0xFFC7C7CC),
        "systemGrey4" => Some(0xFFD1D1D6),
        "systemGrey5" => Some(0xFFE5E5EA),
        "systemGrey6" => Some(0xFFF2F2F7),
        "white" | "systemBackground" => Some(0xFFFFFFFF),
        "black" | "label" => Some(0xFF000000),
        "secondaryLabel" => Some(0x993C3C43),
        "tertiaryLabel" => Some(0x4D3C3C43),
        "quaternaryLabel" => Some(0x2E3C3C43),
        "placeholderText" => Some(0x4D3C3C43),
        "separator" => Some(0x4A3C3C43),
        "opaqueSeparator" => Some(0xFFC6C6C8),
        "link" => Some(0xFF007AFF),
        "secondarySystemBackground" | "systemGroupedBackground" => Some(0xFFF2F2F7),
        "tertiarySystemBackground" => Some(0xFFFFFFFF),
        "lightBackgroundGray" => Some(0xFFE5E5EA),
        "extraLightBackgroundGray" => Some(0xFFEFEFF4),
        "darkBackgroundGray" => Some(0xFF171717),
        "inactiveGray" => Some(0xFF999999),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_comments_and_strings_and_handles_multiline_from_and_underscores() {
        let code = r#"
/* Color(0xFFFF0000) and Colors.red inside block comment */
const label = "Colors.blue and Color(0xFF00FF00)";
const hex = Color(0xFF_21_96_F3);
const argb = Color.fromARGB(0xFF, 0x10, 0x20, 0x30);
const fromAnyOrder = Color.from(
  blue: 0.6,
  green: 0.4,
  alpha: 1.0,
  red: 0.2,
);
final shade = Colors.lightBlue.shade800;
final accent = Colors.deepOrangeAccent[400];
"#;
        let colors = find_dart_colors(code);
        assert_eq!(colors.len(), 5);
        assert!((colors[0].color.blue - (0xF3 as f32 / 255.0)).abs() < 0.01);
        assert!((colors[2].color.red - 0.2).abs() < 0.01);
        assert!((colors[2].color.blue - 0.6).abs() < 0.01);
        // lightBlue.shade800 = 0xFF0277BD
        assert_eq!(lookup_material_color("lightBlue", Some(800)), Some(0xFF0277BD));
        // deepOrangeAccent[400] = 0xFFFF3D00
        assert_eq!(
            lookup_material_color("deepOrangeAccent", Some(400)),
            Some(0xFFFF3D00)
        );
    }

    #[test]
    fn formats_color_presentations_to_dart_hex_and_argb() {
        let range = Range {
            start: Position::new(0, 0),
            end: Position::new(0, 17),
        };
        let pres = build_color_presentations(
            Color {
                red: 1.0,
                green: 0.0,
                blue: 0.5,
                alpha: 1.0,
            },
            range,
        );
        assert_eq!(pres[0].label, "Color(0xFFFF0080)");
        assert_eq!(pres[1].label, "Color.fromARGB(255, 255, 0, 128)");
    }
}
