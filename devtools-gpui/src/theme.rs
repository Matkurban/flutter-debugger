use gpui_kit::{Hsla, Rgba, rgb};

/// Unified dark color tokens matching modern Dark Flutter DevTools & Zed themes.
pub struct DevToolsTheme;

impl DevToolsTheme {
    /// Main workspace background `#16171b`
    pub fn bg_main() -> Rgba {
        rgb(0x16171b)
    }

    /// Panel container background `#1c1e24`
    pub fn bg_panel() -> Rgba {
        rgb(0x1c1e24)
    }

    /// Header / Toolbar / Statusbar background `#22252d`
    pub fn bg_toolbar() -> Rgba {
        rgb(0x22252d)
    }

    /// Elevated surface / dropdown / selected row background `#282c37`
    pub fn bg_elevated() -> Rgba {
        rgb(0x282c37)
    }

    /// Row hover background `#21242d`
    pub fn bg_hover() -> Rgba {
        rgb(0x21242d)
    }

    /// Active selection background `#1f2e4d`
    pub fn bg_selected() -> Rgba {
        rgb(0x1f2e4d)
    }

    /// 1px hairline border / divider `#2d313d`
    pub fn border() -> Rgba {
        rgb(0x2d313d)
    }

    /// Primary accent blue `#5ea2ff`
    pub fn accent_blue() -> Rgba {
        rgb(0x5ea2ff)
    }

    /// Secondary accent cyan `#38bdf8`
    pub fn accent_cyan() -> Rgba {
        rgb(0x38bdf8)
    }

    /// Accent purple (Raster / Memory external / POST) `#c084fc`
    pub fn accent_purple() -> Rgba {
        rgb(0xc084fc)
    }

    /// Success green `#4ade80`
    pub fn success_green() -> Rgba {
        rgb(0x4ade80)
    }

    /// Warning yellow `#facc15`
    pub fn warning_yellow() -> Rgba {
        rgb(0xfacc15)
    }

    /// Error red `#f87171`
    pub fn error_red() -> Rgba {
        rgb(0xf87171)
    }

    /// Primary text `#e5e7eb`
    pub fn text_primary() -> Rgba {
        rgb(0xe5e7eb)
    }

    /// Secondary text `#9ca3af`
    pub fn text_secondary() -> Rgba {
        rgb(0x9ca3af)
    }

    /// Muted text `#6b7280`
    pub fn text_muted() -> Rgba {
        rgb(0x6b7280)
    }

    /// Monospace font family used for code, URLs, JSON, timestamps, and metrics.
    pub const MONO_FONT: &'static str = "Menlo";

    pub fn method_color(method: &str) -> Rgba {
        match method.to_ascii_uppercase().as_str() {
            "GET" => Self::accent_blue(),
            "POST" => Self::success_green(),
            "PUT" | "PATCH" => Self::warning_yellow(),
            "DELETE" => Self::error_red(),
            "WS" | "WSS" => Self::accent_cyan(),
            _ => Self::accent_purple(),
        }
    }

    pub fn status_color(status: Option<u16>) -> Rgba {
        match status {
            Some(200..=299) => Self::success_green(),
            Some(300..=399) => Self::accent_blue(),
            Some(400..=499) => Self::warning_yellow(),
            Some(500..=599) => Self::error_red(),
            Some(_) => Self::text_secondary(),
            None => Self::warning_yellow(),
        }
    }

    pub fn to_hsla(color: Rgba, alpha: f32) -> Hsla {
        let mut hsla: Hsla = color.into();
        hsla.a = alpha;
        hsla
    }
}

/// Formats byte counts into compact human-readable units (`B`, `KB`, `MB`).
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if (bytes as f64) >= MB {
        format!("{:.1} MB", (bytes as f64) / MB)
    } else if (bytes as f64) >= KB {
        format!("{:.1} KB", (bytes as f64) / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Formats a millisecond duration nicely.
pub fn format_duration_ms(duration_ms: Option<u64>) -> String {
    match duration_ms {
        Some(ms) if ms >= 1000 => format!("{:.2} s", (ms as f64) / 1000.0),
        Some(ms) => format!("{ms} ms"),
        None => "Pending".to_string(),
    }
}

/// Formats current system time into the user's local timezone `HH:MM:SS`.
pub fn current_local_hms() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format_epoch_millis_local_hms(millis, false)
}

/// Formats a microsecond epoch timestamp into the user's local timezone `HH:MM:SS.mmm`.
pub fn format_micros_local_hms(micros: u64) -> String {
    if micros == 0 {
        return current_local_hms();
    }
    // Dart VM timeline timestamps can be either monotonic microseconds since boot or epoch microseconds.
    let millis = micros / 1000;
    format_epoch_millis_local_hms(millis, true)
}

/// Converts an epoch millisecond timestamp to local timezone `HH:MM:SS` or `HH:MM:SS.mmm`.
pub fn format_epoch_millis_local_hms(millis: u64, include_millis: bool) -> String {
    let secs = (millis / 1000) as i64;
    let ms = millis % 1000;

    #[cfg(unix)]
    {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let time_t_val = secs as libc::time_t;
        let ptr = unsafe { libc::localtime_r(&time_t_val, &mut tm) };
        if !ptr.is_null() {
            let h = tm.tm_hour.clamp(0, 23);
            let m = tm.tm_min.clamp(0, 59);
            let s = tm.tm_sec.clamp(0, 60);
            return if include_millis {
                format!("{h:02}:{m:02}:{s:02}.{ms:03}")
            } else {
                format!("{h:02}:{m:02}:{s:02}")
            };
        }
    }

    let s = (secs.rem_euclid(60)) as u64;
    let m = ((secs / 60).rem_euclid(60)) as u64;
    let h = ((secs / 3600).rem_euclid(24)) as u64;
    if include_millis {
        format!("{h:02}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}
