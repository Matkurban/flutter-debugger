use std::sync::OnceLock;

use regex::Regex;
use tower_lsp_server::ls_types::{Color, ColorInformation, ColorPresentation, Position, Range, TextEdit};

/// Extracts all Flutter color literals and Material / Cupertino color constants from a Dart file
/// for LSP `textDocument/documentColor`.
pub fn find_dart_colors(content: &str) -> Vec<ColorInformation> {
    static HEX_RE: OnceLock<Regex> = OnceLock::new();
    static ARGB_RE: OnceLock<Regex> = OnceLock::new();
    static RGBO_RE: OnceLock<Regex> = OnceLock::new();
    static FROM_RE: OnceLock<Regex> = OnceLock::new();
    static MATERIAL_RE: OnceLock<Regex> = OnceLock::new();
    static CUPERTINO_RE: OnceLock<Regex> = OnceLock::new();

    let hex_re = HEX_RE.get_or_init(|| {
        Regex::new(r"\bColor\s*\(\s*0[xX]([0-9a-fA-F]{8})\s*\)").expect("valid hex color regex")
    });
    let argb_re = ARGB_RE.get_or_init(|| {
        Regex::new(r"\bColor\.fromARGB\s*\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)")
            .expect("valid argb regex")
    });
    let rgbo_re = RGBO_RE.get_or_init(|| {
        Regex::new(
            r"\bColor\.fromRGBO\s*\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([0-9]*\.?[0-9]+)\s*\)",
        )
        .expect("valid rgbo regex")
    });
    let from_re = FROM_RE.get_or_init(|| {
        Regex::new(
            r"\bColor\.from\s*\(\s*alpha:\s*([0-9]*\.?[0-9]+)\s*,\s*red:\s*([0-9]*\.?[0-9]+)\s*,\s*green:\s*([0-9]*\.?[0-9]+)\s*,\s*blue:\s*([0-9]*\.?[0-9]+)\s*,?\s*\)",
        )
        .expect("valid Color.from regex")
    });
    let material_re = MATERIAL_RE.get_or_init(|| {
        Regex::new(r"\bColors\.([a-zA-Z0-9]+)(?:\[(\d+)\]|\.shade(\d+))?")
            .expect("valid material regex")
    });
    let cupertino_re = CUPERTINO_RE.get_or_init(|| {
        Regex::new(r"\bCupertinoColors\.([a-zA-Z0-9]+)").expect("valid cupertino regex")
    });

    let mut results = Vec::new();

    for (line_idx, raw_line) in content.lines().enumerate() {
        let code_line = strip_line_comment(raw_line);
        if code_line.trim().is_empty() {
            continue;
        }
        let line = line_idx as u32;

        // 1. Color(0xAARRGGBB)
        for cap in hex_re.captures_iter(code_line) {
            if let (Some(full), Some(hex)) = (cap.get(0), cap.get(1))
                && let Ok(argb) = u32::from_str_radix(hex.as_str(), 16)
            {
                results.push(ColorInformation {
                    range: byte_span_to_range(code_line, line, full.start(), full.end()),
                    color: argb_to_lsp_color(argb),
                });
            }
        }

        // 2. Color.fromARGB(a, r, g, b)
        for cap in argb_re.captures_iter(code_line) {
            if let (Some(full), Some(a), Some(r), Some(g), Some(b)) =
                (cap.get(0), cap.get(1), cap.get(2), cap.get(3), cap.get(4))
                && let (Ok(a), Ok(r), Ok(g), Ok(b)) = (
                    a.as_str().parse::<u8>(),
                    r.as_str().parse::<u8>(),
                    g.as_str().parse::<u8>(),
                    b.as_str().parse::<u8>(),
                )
            {
                results.push(ColorInformation {
                    range: byte_span_to_range(code_line, line, full.start(), full.end()),
                    color: rgba_u8_to_lsp_color(r, g, b, a),
                });
            }
        }

        // 3. Color.fromRGBO(r, g, b, opacity)
        for cap in rgbo_re.captures_iter(code_line) {
            if let (Some(full), Some(r), Some(g), Some(b), Some(o)) =
                (cap.get(0), cap.get(1), cap.get(2), cap.get(3), cap.get(4))
                && let (Ok(r), Ok(g), Ok(b), Ok(o)) = (
                    r.as_str().parse::<u8>(),
                    g.as_str().parse::<u8>(),
                    b.as_str().parse::<u8>(),
                    o.as_str().parse::<f32>(),
                )
            {
                results.push(ColorInformation {
                    range: byte_span_to_range(code_line, line, full.start(), full.end()),
                    color: Color {
                        red: f32::from(r) / 255.0,
                        green: f32::from(g) / 255.0,
                        blue: f32::from(b) / 255.0,
                        alpha: o.clamp(0.0, 1.0),
                    },
                });
            }
        }

        // 4. Color.from(alpha: ..., red: ..., green: ..., blue: ...)
        for cap in from_re.captures_iter(code_line) {
            if let (Some(full), Some(a), Some(r), Some(g), Some(b)) =
                (cap.get(0), cap.get(1), cap.get(2), cap.get(3), cap.get(4))
                && let (Ok(a), Ok(r), Ok(g), Ok(b)) = (
                    a.as_str().parse::<f32>(),
                    r.as_str().parse::<f32>(),
                    g.as_str().parse::<f32>(),
                    b.as_str().parse::<f32>(),
                )
            {
                results.push(ColorInformation {
                    range: byte_span_to_range(code_line, line, full.start(), full.end()),
                    color: Color {
                        red: r.clamp(0.0, 1.0),
                        green: g.clamp(0.0, 1.0),
                        blue: b.clamp(0.0, 1.0),
                        alpha: a.clamp(0.0, 1.0),
                    },
                });
            }
        }

        // 5. Material Colors.<name>([shade] | .shade<N>)
        for cap in material_re.captures_iter(code_line) {
            if let (Some(full), Some(name)) = (cap.get(0), cap.get(1)) {
                let shade = cap
                    .get(2)
                    .or_else(|| cap.get(3))
                    .and_then(|m| m.as_str().parse::<u16>().ok());
                if let Some(argb) = lookup_material_color(name.as_str(), shade) {
                    results.push(ColorInformation {
                        range: byte_span_to_range(code_line, line, full.start(), full.end()),
                        color: argb_to_lsp_color(argb),
                    });
                }
            }
        }

        // 6. CupertinoColors.<name>
        for cap in cupertino_re.captures_iter(code_line) {
            if let (Some(full), Some(name)) = (cap.get(0), cap.get(1))
                && let Some(argb) = lookup_cupertino_color(name.as_str())
            {
                results.push(ColorInformation {
                    range: byte_span_to_range(code_line, line, full.start(), full.end()),
                    color: argb_to_lsp_color(argb),
                });
            }
        }
    }

    results
}

/// Builds canonical Dart `Color(0xAARRGGBB)` and `Color.fromARGB(...)` presentations
/// for LSP `textDocument/colorPresentation`.
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

fn byte_span_to_range(line_text: &str, line: u32, start_byte: usize, end_byte: usize) -> Range {
    let start_col = line_text[..start_byte].encode_utf16().count() as u32;
    let end_col = line_text[..end_byte].encode_utf16().count() as u32;
    Range {
        start: Position::new(line, start_col),
        end: Position::new(line, end_col),
    }
}

fn strip_line_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'/' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

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

    let s = shade.unwrap_or(500);
    match (name, s) {
        ("red", 50) => Some(0xFFFF_EBEE),
        ("red", 100) => Some(0xFFFF_CDD2),
        ("red", 200) => Some(0xFFEF_9A9A),
        ("red", 300) => Some(0xFFE5_7373),
        ("red", 400) => Some(0xFFEF_5350),
        ("red", 500) => Some(0xFFF4_4336),
        ("red", 600) => Some(0xFFE5_3935),
        ("red", 700) => Some(0xFFD3_2F2F),
        ("red", 800) => Some(0xFFC6_2828),
        ("red", 900) => Some(0xFFB7_1C1C),
        ("redAccent", 100) => Some(0xFFFF_8A80),
        ("redAccent", 200 | 500) => Some(0xFFFF_5252),
        ("redAccent", 400) => Some(0xFFFF_1744),
        ("redAccent", 700) => Some(0xFFD5_0000),

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
        ("pinkAccent", _) => Some(0xFFFF4081),

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
        ("purpleAccent", _) => Some(0xFFE040FB),

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
        ("deepPurpleAccent", _) => Some(0xFF7C4DFF),

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
        ("indigoAccent", _) => Some(0xFF536DFE),

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
        ("blueAccent", 200 | 500) => Some(0xFF448AFF),
        ("blueAccent", 400) => Some(0xFF2979FF),
        ("blueAccent", 700) => Some(0xFF2962FF),

        ("lightBlue", 500) => Some(0xFF03A9F4),
        ("lightBlue", 300) => Some(0xFF4FC3F7),
        ("lightBlue", 700) => Some(0xFF0288D1),
        ("lightBlueAccent", _) => Some(0xFF40C4FF),

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
        ("cyanAccent", _) => Some(0xFF18FFFF),

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
        ("tealAccent", _) => Some(0xFF64FFDA),

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
        ("greenAccent", _) => Some(0xFF69F0AE),

        ("lightGreen", 500) => Some(0xFF8BC34A),
        ("lightGreenAccent", _) => Some(0xFFB2FF59),
        ("lime", 500) => Some(0xFFCDDC39),
        ("limeAccent", _) => Some(0xFFEEFF41),

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
        ("yellowAccent", _) => Some(0xFFFFFF00),

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
        ("amberAccent", _) => Some(0xFFFFD740),

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
        ("orangeAccent", _) => Some(0xFFFFAB40),

        ("deepOrange", 500) => Some(0xFFFF5722),
        ("deepOrangeAccent", _) => Some(0xFFFF6E40),

        ("brown", 500) => Some(0xFF795548),
        ("brown", 300) => Some(0xFFA1887F),
        ("brown", 700) => Some(0xFF5D4037),

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
    fn parses_hex_argb_rgbo_from_material_and_cupertino_colors() {
        let code = r#"
const c1 = Color(0xFF2196F3);
const c2 = Color.fromARGB(255, 10, 20, 30);
const c3 = Color.fromRGBO(255, 0, 128, 0.5);
const c4 = Color.from(alpha: 1.0, red: 0.2, green: 0.4, blue: 0.6);
const c5 = Colors.blue;
final c6 = Colors.grey[700];
final c7 = Colors.red.shade900;
const c8 = CupertinoColors.activeBlue;
// Color(0xFF000000) commented out
"#;
        let colors = find_dart_colors(code);
        assert_eq!(colors.len(), 8);
        assert!((colors[0].color.blue - (0xF3 as f32 / 255.0)).abs() < 0.01);
        assert!((colors[2].color.alpha - 0.5).abs() < 0.01);
        assert!((colors[7].color.blue - 1.0).abs() < 0.01);
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
