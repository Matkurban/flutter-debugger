/// Colorizes DAP `output` event text using standard ANSI 16-color SGR codes so Zed's
/// Debug Console (`StyledAnsiTextHandler`) renders multi-state colors matching VS Code's
/// Debug Console while adapting automatically to light and dark Zed themes.
///
/// Key behaviors:
/// 1. Safely parses CSI SGR sequences (`\x1b[<params>m`), skipping extended color sub-params
///    (`38;5;N`, `38;2;R;G;B`, `48;5;N`, `48;2;R;G;B`, `58;...`) so `2` in `38;2;...` is never
///    mistaken for Dim. Converts standalone SGR `2` (Dim, unsupported by Zed Debug Console) to `90`
///    (`BrightBlack` / muted dark gray) and converts a matching `22` (when `dim_active` is true)
///    to `39` (default foreground).
/// 2. Tracks active foreground/background ANSI state across lines (`fg_active` / `bg_active`) so
///    multi-line colored blocks are preserved without mid-block color overrides or premature resets.
/// 3. Classifies uncolored non-blank lines by DAP `category` (`stderr`, `important`, `console`,
///    `stdout` / `None`) and Flutter/Dart log semantics, wrapping each line body before `\r\n`/`\n`.
pub fn colorize_dap_output(text: &str, category: Option<&str>) -> String {
    if text.is_empty() {
        return String::new();
    }

    let normalized = normalize_dim_sgr_sequences(text);
    let mut out = String::with_capacity(normalized.len() + 32);
    let mut cursor = 0usize;
    let mut fg_active = false;
    let mut bg_active = false;

    while cursor < normalized.len() {
        let rest = &normalized[cursor..];
        let (line_body, line_ending, consumed) = match rest.find('\n') {
            Some(nl_idx) => {
                if nl_idx > 0 && rest.as_bytes()[nl_idx - 1] == b'\r' {
                    (&rest[..nl_idx - 1], "\r\n", nl_idx + 1)
                } else {
                    (&rest[..nl_idx], "\n", nl_idx + 1)
                }
            }
            None => (rest, "", rest.len()),
        };

        if line_body.trim().is_empty() {
            out.push_str(line_body);
            out.push_str(line_ending);
        } else if fg_active || bg_active || line_has_explicit_ansi_color(line_body) {
            out.push_str(line_body);
            out.push_str(line_ending);
            update_ansi_color_state(line_body, &mut fg_active, &mut bg_active);
        } else {
            let code = classify_line_ansi_color(line_body, category);
            out.push_str("\x1b[");
            out.push_str(code);
            out.push('m');
            out.push_str(line_body);
            out.push_str("\x1b[0m");
            out.push_str(line_ending);
        }

        cursor += consumed;
    }

    out
}

/// Normalizes CSI SGR sequences in `text`:
/// - Converts standalone SGR `2` (Dim/Faint) to `90` (`BrightBlack`) when the SGR sequence
///   does not already specify an explicit foreground color, setting `dim_active = true`.
/// - Converts SGR `22` (Normal intensity) to `39` (Default foreground) only when `dim_active`
///   is currently true and the sequence does not already specify an explicit foreground or reset.
/// - Properly skips sub-parameters of `38;5;N`, `38;2;R;G;B`, `48;5;N`, `48;2;R;G;B`, and `58;...`.
fn normalize_dim_sgr_sequences(text: &str) -> String {
    if !text.contains("\x1b[") {
        return text.to_string();
    }

    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut dim_active = false;

    while cursor < bytes.len() {
        let Some(rel_esc) = text[cursor..].find("\x1b[") else {
            out.push_str(&text[cursor..]);
            break;
        };
        let esc_start = cursor + rel_esc;
        out.push_str(&text[cursor..esc_start]);

        let params_start = esc_start + 2;
        let mut idx = params_start;
        while idx < bytes.len() && (bytes[idx].is_ascii_digit() || bytes[idx] == b';') {
            idx += 1;
        }

        if idx < bytes.len() && bytes[idx] == b'm' {
            let params_str = &text[params_start..idx];
            let rewritten_params = rewrite_sgr_params(params_str, &mut dim_active);
            out.push_str("\x1b[");
            out.push_str(&rewritten_params);
            out.push('m');
            cursor = idx + 1;
        } else {
            out.push_str("\x1b[");
            cursor = params_start;
        }
    }

    out
}

fn rewrite_sgr_params(params_str: &str, dim_active: &mut bool) -> String {
    if params_str.is_empty() {
        *dim_active = false;
        return String::new();
    }

    let tokens: Vec<&str> = params_str.split(';').collect();
    let mut has_explicit_fg = false;
    let mut has_reset_or_default_fg = false;

    let mut i = 0usize;
    while i < tokens.len() {
        let code = tokens[i].parse::<u16>().unwrap_or(0);
        if matches!(code, 38 | 48 | 58) {
            if code == 38 {
                has_explicit_fg = true;
            }
            let step = extended_color_token_len(&tokens, i);
            i += step;
            continue;
        }
        if matches!(code, 30..=37 | 90..=97) {
            has_explicit_fg = true;
        } else if code == 0 || code == 39 {
            has_reset_or_default_fg = true;
        }
        i += 1;
    }

    let mut out_tokens: Vec<&str> = Vec::with_capacity(tokens.len());
    let mut j = 0usize;
    while j < tokens.len() {
        let raw = tokens[j];
        let code = raw.parse::<u16>().unwrap_or(0);
        if matches!(code, 38 | 48 | 58) {
            if code == 38 {
                *dim_active = false;
            }
            let step = extended_color_token_len(&tokens, j);
            for &tok in &tokens[j..j + step] {
                out_tokens.push(tok);
            }
            j += step;
            continue;
        }
        if code == 2 && !has_explicit_fg {
            out_tokens.push("90");
            *dim_active = true;
        } else if code == 22 && *dim_active && !has_explicit_fg && !has_reset_or_default_fg {
            out_tokens.push("39");
            *dim_active = false;
        } else {
            if matches!(code, 0 | 39 | 30..=37 | 90..=97) {
                *dim_active = false;
            }
            out_tokens.push(raw);
        }
        j += 1;
    }

    out_tokens.join(";")
}

fn extended_color_token_len(tokens: &[&str], start: usize) -> usize {
    let Some(mode) = tokens.get(start + 1).and_then(|s| s.parse::<u16>().ok()) else {
        return 1;
    };
    match mode {
        5 => 3.min(tokens.len() - start),
        2 => 5.min(tokens.len() - start),
        _ => 2.min(tokens.len() - start),
    }
}

fn line_has_explicit_ansi_color(line: &str) -> bool {
    if !line.contains("\x1b[") {
        return false;
    }

    let bytes = line.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let Some(rel_esc) = line[cursor..].find("\x1b[") else {
            break;
        };
        let params_start = cursor + rel_esc + 2;
        let mut idx = params_start;
        while idx < bytes.len() && (bytes[idx].is_ascii_digit() || bytes[idx] == b';') {
            idx += 1;
        }
        if idx < bytes.len() && bytes[idx] == b'm' {
            let params_str = &line[params_start..idx];
            if sgr_params_have_explicit_color(params_str) {
                return true;
            }
            cursor = idx + 1;
        } else {
            cursor = params_start;
        }
    }

    false
}

fn sgr_params_have_explicit_color(params_str: &str) -> bool {
    if params_str.is_empty() {
        return false;
    }
    let tokens: Vec<&str> = params_str.split(';').collect();
    let mut i = 0usize;
    while i < tokens.len() {
        let Ok(code) = tokens[i].parse::<u16>() else {
            i += 1;
            continue;
        };
        if code == 38 || code == 48 {
            return true;
        }
        if code == 58 {
            i += extended_color_token_len(&tokens, i);
            continue;
        }
        if matches!(code, 30..=37 | 40..=47 | 90..=97 | 100..=107) {
            return true;
        }
        i += 1;
    }
    false
}

fn update_ansi_color_state(line: &str, fg_active: &mut bool, bg_active: &mut bool) {
    if !line.contains("\x1b[") {
        return;
    }

    let bytes = line.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let Some(rel_esc) = line[cursor..].find("\x1b[") else {
            break;
        };
        let params_start = cursor + rel_esc + 2;
        let mut idx = params_start;
        while idx < bytes.len() && (bytes[idx].is_ascii_digit() || bytes[idx] == b';') {
            idx += 1;
        }
        if idx < bytes.len() && bytes[idx] == b'm' {
            let params_str = &line[params_start..idx];
            if params_str.is_empty() {
                *fg_active = false;
                *bg_active = false;
            } else {
                let tokens: Vec<&str> = params_str.split(';').collect();
                let mut i = 0usize;
                while i < tokens.len() {
                    let code = tokens[i].parse::<u16>().unwrap_or(0);
                    if code == 38 {
                        *fg_active = true;
                        i += extended_color_token_len(&tokens, i);
                        continue;
                    }
                    if code == 48 {
                        *bg_active = true;
                        i += extended_color_token_len(&tokens, i);
                        continue;
                    }
                    if code == 58 {
                        i += extended_color_token_len(&tokens, i);
                        continue;
                    }
                    match code {
                        0 => {
                            *fg_active = false;
                            *bg_active = false;
                        }
                        30..=37 | 90..=97 => *fg_active = true,
                        39 => *fg_active = false,
                        40..=47 | 100..=107 => *bg_active = true,
                        49 => *bg_active = false,
                        _ => {}
                    }
                    i += 1;
                }
            }
            cursor = idx + 1;
        } else {
            cursor = params_start;
        }
    }
}

fn classify_line_ansi_color(line_body: &str, category: Option<&str>) -> &'static str {
    let trimmed = line_body.trim();

    match category {
        Some("stderr") => "31",
        Some("important") => "33",
        Some("console") => classify_console_line(trimmed),
        Some("stdout") | None | Some(_) => classify_stdout_line(trimmed),
    }
}

fn classify_console_line(trimmed: &str) -> &'static str {
    if trimmed.starts_with("Failed to ")
        || trimmed.starts_with("Error:")
        || trimmed.starts_with("ERROR:")
        || trimmed.starts_with("FAILURE:")
        || trimmed.starts_with("Unhandled exception")
        || trimmed.starts_with("Exception:")
        || trimmed.contains("═══")
    {
        return "31";
    }

    if trimmed.starts_with("Reloaded ")
        || trimmed.starts_with("Restarted application")
        || trimmed.starts_with("✓ ")
        || trimmed.starts_with("Application finished.")
    {
        return "32";
    }

    if trimmed.starts_with("[flutter-debugger]")
        || trimmed.starts_with("A Dart VM Service")
        || trimmed.starts_with("The Flutter DevTools")
        || trimmed.starts_with("An Observatory")
        || trimmed.starts_with("Flutter DevTools")
    {
        return "36";
    }

    "33"
}

fn classify_stdout_line(trimmed: &str) -> &'static str {
    let payload = trimmed
        .strip_prefix("flutter:")
        .map(str::trim_start)
        .unwrap_or(trimmed);

    // 1. Error lines -> Red (31)
    if trimmed.starts_with("E/")
        || trimmed.starts_with("[ERROR:")
        || trimmed.contains("[ERROR:")
        || trimmed.starts_with("FATAL")
        || trimmed.starts_with("Unhandled Exception:")
        || trimmed.starts_with("Unhandled exception:")
        || trimmed.starts_with("Exception:")
        || payload.starts_with("Unhandled Exception:")
        || payload.starts_with("Unhandled exception:")
        || payload.starts_with("Exception:")
        || trimmed.contains("═══")
    {
        return "31";
    }

    // 2. Warning lines -> Yellow (33)
    if trimmed.starts_with("W/")
        || trimmed.starts_with("[WARNING:")
        || trimmed.contains("[WARNING:")
        || trimmed.starts_with("Warning:")
        || trimmed.starts_with("WARNING:")
        || payload.starts_with("Warning:")
        || payload.starts_with("WARNING:")
    {
        return "33";
    }

    // 3. Success milestones -> Green (32)
    if trimmed.starts_with("Reloaded ")
        || trimmed.starts_with("Restarted application")
        || trimmed.starts_with("✓ ")
        || payload.starts_with("Reloaded ")
        || payload.starts_with("Restarted application")
        || payload.starts_with("✓ ")
    {
        return "32";
    }

    // 4. Debug/Verbose logs, async gap markers, and internal framework/pub-cache stack frames -> Dark Gray (90)
    if trimmed.starts_with("D/")
        || trimmed.starts_with("V/")
        || trimmed.starts_with("[DEBUG:")
        || trimmed.starts_with("[VERBOSE:")
        || trimmed.contains("[DEBUG:")
        || trimmed.contains("[VERBOSE:")
        || trimmed == "<asynchronous suspension>"
        || payload == "<asynchronous suspension>"
        || is_framework_stack_frame(trimmed)
        || is_framework_stack_frame(payload)
    {
        return "90";
    }

    // 5. General info logs (`I/flutter`, `flutter:`, user stack frames, widget details, etc.) -> Blue (34)
    "34"
}

fn is_framework_stack_frame(s: &str) -> bool {
    let Some(after_hash) = s.strip_prefix('#') else {
        return false;
    };
    if !after_hash.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    s.contains("(dart:")
        || s.contains("(package:flutter/")
        || s.contains("/packages/flutter/")
        || s.contains("/.pub-cache/")
        || s.contains("/pub-cache/")
        || s.contains("/pub_cache/")
        || s.contains("/Pub/Cache/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorizes_stderr_important_and_console_categories_by_severity() {
        // stderr -> red (31)
        let stderr_text = "════════ Exception caught by widgets library ════════\nThe following assertion was thrown:\n═════════════════════════════════════════════════════\n";
        let colored_stderr = colorize_dap_output(stderr_text, Some("stderr"));
        assert_eq!(
            colored_stderr,
            "\x1b[31m════════ Exception caught by widgets library ════════\x1b[0m\n\x1b[31mThe following assertion was thrown:\x1b[0m\n\x1b[31m═════════════════════════════════════════════════════\x1b[0m\n"
        );

        // important -> yellow (33)
        assert_eq!(
            colorize_dap_output("Important notification\n", Some("important")),
            "\x1b[33mImportant notification\x1b[0m\n"
        );

        // console -> build yellow (33), success green (32), link cyan (36), failure red (31)
        let console_text = concat!(
            "Launching lib/main.dart on macOS in debug mode...\n",
            "Running Xcode build...\n",
            "✓ Built build/macos/Build/Products/Debug/app.app\n",
            "Reloaded 1 of 742 libraries in 143ms.\n",
            "[flutter-debugger] Auto-selected device macos\n",
            "The Flutter DevTools debugger and profiler is available at: http://127.0.0.1:9100\n",
            "Failed to launch emulator: process exited with code 1\n"
        );
        let colored_console = colorize_dap_output(console_text, Some("console"));
        let lines: Vec<&str> = colored_console.lines().collect();
        assert_eq!(
            lines[0],
            "\x1b[33mLaunching lib/main.dart on macOS in debug mode...\x1b[0m"
        );
        assert_eq!(lines[1], "\x1b[33mRunning Xcode build...\x1b[0m");
        assert_eq!(
            lines[2],
            "\x1b[32m✓ Built build/macos/Build/Products/Debug/app.app\x1b[0m"
        );
        assert_eq!(
            lines[3],
            "\x1b[32mReloaded 1 of 742 libraries in 143ms.\x1b[0m"
        );
        assert_eq!(
            lines[4],
            "\x1b[36m[flutter-debugger] Auto-selected device macos\x1b[0m"
        );
        assert_eq!(
            lines[5],
            "\x1b[36mThe Flutter DevTools debugger and profiler is available at: http://127.0.0.1:9100\x1b[0m"
        );
        assert_eq!(
            lines[6],
            "\x1b[31mFailed to launch emulator: process exited with code 1\x1b[0m"
        );
    }

    #[test]
    fn colorizes_multiline_stdout_block_by_log_level_and_stack_frame_origin() {
        let stdout_block = concat!(
            "I/flutter (12345): User logged in\n",
            "W/flutter (12345): Deprecated API usage\n",
            "E/flutter (12345): Network request failed\n",
            "D/flutter (12345): Verbose payload trace\n",
            "#0      _HomeState.build (zed://file/Users/dev/my_app/lib/pages/home.dart:42:5)\n",
            "#1      ComponentElement.performRebuild (zed://file/Users/dev/flutter/packages/flutter/lib/src/widgets/framework.dart:5120:14)\n",
            "#2      _rootRun (dart:async/zone.dart:1399:13)\n",
            "<asynchronous suspension>\n",
            "#3      Provider.of (zed://file/C:/Users/dev/AppData/Local/Pub/Cache/hosted/pub.dev/provider-6.1.0/lib/src/provider.dart:19:7)\n"
        );
        let colored = colorize_dap_output(stdout_block, Some("stdout"));
        let lines: Vec<&str> = colored.lines().collect();
        assert_eq!(lines[0], "\x1b[34mI/flutter (12345): User logged in\x1b[0m");
        assert_eq!(
            lines[1],
            "\x1b[33mW/flutter (12345): Deprecated API usage\x1b[0m"
        );
        assert_eq!(
            lines[2],
            "\x1b[31mE/flutter (12345): Network request failed\x1b[0m"
        );
        assert_eq!(
            lines[3],
            "\x1b[90mD/flutter (12345): Verbose payload trace\x1b[0m"
        );
        assert_eq!(
            lines[4],
            "\x1b[34m#0      _HomeState.build (zed://file/Users/dev/my_app/lib/pages/home.dart:42:5)\x1b[0m"
        );
        assert_eq!(
            lines[5],
            "\x1b[90m#1      ComponentElement.performRebuild (zed://file/Users/dev/flutter/packages/flutter/lib/src/widgets/framework.dart:5120:14)\x1b[0m"
        );
        assert_eq!(
            lines[6],
            "\x1b[90m#2      _rootRun (dart:async/zone.dart:1399:13)\x1b[0m"
        );
        assert_eq!(lines[7], "\x1b[90m<asynchronous suspension>\x1b[0m");
        assert_eq!(
            lines[8],
            "\x1b[90m#3      Provider.of (zed://file/C:/Users/dev/AppData/Local/Pub/Cache/hosted/pub.dev/provider-6.1.0/lib/src/provider.dart:19:7)\x1b[0m"
        );
    }

    #[test]
    fn converts_ansi_dim_sgr_to_bright_black_without_corrupting_bold_22() {
        let dim_frame =
            "\x1b[2m#1      Element.rebuild (zed://file/Users/dev/flutter/packages/flutter/lib/framework.dart:10:2)\x1b[0m\n";
        assert_eq!(
            colorize_dap_output(dim_frame, Some("stdout")),
            "\x1b[90m#1      Element.rebuild (zed://file/Users/dev/flutter/packages/flutter/lib/framework.dart:10:2)\x1b[0m\n"
        );

        let dim_inline = "\x1b[2mMuted prefix\x1b[22m\n";
        assert_eq!(
            colorize_dap_output(dim_inline, Some("stdout")),
            "\x1b[90mMuted prefix\x1b[39m\n"
        );

        // Bold (`\x1b[1m`) closed by `\x1b[22m` must NOT be converted to `\x1b[39m`, and the line gets stdout blue (`\x1b[34m`)
        let bold_inline = "\x1b[1mBold\x1b[22m normal\n";
        assert_eq!(
            colorize_dap_output(bold_inline, Some("stdout")),
            "\x1b[34m\x1b[1mBold\x1b[22m normal\x1b[0m\n"
        );
    }

    #[test]
    fn preserves_multiline_ansi_blocks_and_resumes_coloring_after_reset() {
        let multiline_box = "\x1b[33m┌──\n│ warn\n└──\x1b[0m\nNext stdout\n";
        assert_eq!(
            colorize_dap_output(multiline_box, Some("stdout")),
            "\x1b[33m┌──\n│ warn\n└──\x1b[0m\n\x1b[34mNext stdout\x1b[0m\n"
        );

        let multiline_dim_frames = "\x1b[2m#1      FrameOne (dart:async/zone.dart:10:2)\n#2      FrameTwo (dart:async/zone.dart:20:4)\x1b[0m\n";
        assert_eq!(
            colorize_dap_output(multiline_dim_frames, Some("stdout")),
            "\x1b[90m#1      FrameOne (dart:async/zone.dart:10:2)\n#2      FrameTwo (dart:async/zone.dart:20:4)\x1b[0m\n"
        );
    }

    #[test]
    fn preserves_existing_explicit_ansi_colors_including_truecolor_without_corrupting_38_2() {
        let ansi_16 = "\x1b[35m[Logger] Custom magenta message\x1b[0m\n";
        assert_eq!(colorize_dap_output(ansi_16, Some("stdout")), ansi_16);

        // TrueColor `38;2;255;128;0` (and even `38;2;2;22;2`) must NEVER have `2` or `22` replaced!
        let truecolor = "\x1b[38;2;255;128;0mTrueColor orange\x1b[0m\n\x1b[38;2;2;22;2mDark green RGB(2,22,2)\x1b[0m\n";
        assert_eq!(colorize_dap_output(truecolor, Some("stdout")), truecolor);

        // Underline color `58;2;35;0;0` does not count as an explicit foreground/background color
        let underline_only = "\x1b[58;2;35;0;0mUnderlined info\x1b[0m\n";
        assert_eq!(
            colorize_dap_output(underline_only, Some("stdout")),
            "\x1b[34m\x1b[58;2;35;0;0mUnderlined info\x1b[0m\x1b[0m\n"
        );
    }

    #[test]
    fn preserves_crlf_and_lf_line_endings_and_leaves_blank_lines_untouched() {
        let input = "First line\r\n\r\n   \nSecond line\n";
        let colored = colorize_dap_output(input, Some("stdout"));
        assert_eq!(
            colored,
            "\x1b[34mFirst line\x1b[0m\r\n\r\n   \n\x1b[34mSecond line\x1b[0m\n"
        );
    }
}
