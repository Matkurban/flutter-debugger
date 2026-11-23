use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Supported Flutter Service Extension overlays & runtime toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceOverlay {
    Inspector,
    Performance,
    DebugPaint,
    Baselines,
    RepaintRainbow,
    SlowAnimations,
    DebugBanner,
    OversizedImages,
    PlatformOverride,
    BrightnessOverride,
}

impl ServiceOverlay {
    pub const fn all() -> [Self; 10] {
        [
            Self::Inspector,
            Self::Performance,
            Self::DebugPaint,
            Self::Baselines,
            Self::RepaintRainbow,
            Self::SlowAnimations,
            Self::DebugBanner,
            Self::OversizedImages,
            Self::PlatformOverride,
            Self::BrightnessOverride,
        ]
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "inspector" | "widget-inspector" | "widgetinspector" => Some(Self::Inspector),
            "performance" | "performance-overlay" | "performanceoverlay" => Some(Self::Performance),
            "debug-paint" | "debugpaint" | "paint" => Some(Self::DebugPaint),
            "baselines" | "paint-baselines" | "debugpaintbaselines" => Some(Self::Baselines),
            "repaint" | "repaint-rainbow" | "repaintrainbow" => Some(Self::RepaintRainbow),
            "slow-anim" | "slow-animations" | "slowanimations" | "slow" => {
                Some(Self::SlowAnimations)
            }
            "banner" | "debug-banner" | "debugbanner" => Some(Self::DebugBanner),
            "oversized" | "oversized-images" | "invert-oversized-images" => {
                Some(Self::OversizedImages)
            }
            "platform" | "platform-override" | "platformoverride" => Some(Self::PlatformOverride),
            "brightness" | "brightness-override" | "brightnessoverride" | "theme" => {
                Some(Self::BrightnessOverride)
            }
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Inspector => "Widget Inspector",
            Self::Performance => "Performance Overlay",
            Self::DebugPaint => "Debug Paint",
            Self::Baselines => "Paint Baselines",
            Self::RepaintRainbow => "Repaint Rainbow",
            Self::SlowAnimations => "Slow Animations",
            Self::DebugBanner => "Debug Banner",
            Self::OversizedImages => "Invert Oversized Images",
            Self::PlatformOverride => "Target Platform",
            Self::BrightnessOverride => "Brightness",
        }
    }

    pub fn extension_method(self) -> &'static str {
        match self {
            Self::Inspector => "ext.flutter.inspector.show",
            Self::Performance => "ext.flutter.showPerformanceOverlay",
            Self::DebugPaint => "ext.flutter.debugPaint",
            Self::Baselines => "ext.flutter.debugPaintBaselinesEnabled",
            Self::RepaintRainbow => "ext.flutter.repaintRainbow",
            Self::SlowAnimations => "ext.flutter.timeDilation",
            Self::DebugBanner => "ext.flutter.debugAllowBanner",
            Self::OversizedImages => "ext.flutter.invertOversizedImages",
            Self::PlatformOverride => "ext.flutter.platformOverride",
            Self::BrightnessOverride => "ext.flutter.brightnessOverride",
        }
    }
}

/// Tracks toggle state for Flutter Service Extensions within an active session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayState {
    pub inspector: bool,
    pub performance: bool,
    pub debug_paint: bool,
    pub baselines: bool,
    pub repaint_rainbow: bool,
    pub slow_animations: bool,
    pub debug_banner: bool,
    pub oversized_images: bool,
    pub platform_index: usize,
    pub platform_override: String,
    pub dark_brightness: bool,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            inspector: false,
            performance: false,
            debug_paint: false,
            baselines: false,
            repaint_rainbow: false,
            slow_animations: false,
            debug_banner: true,
            oversized_images: false,
            platform_index: 0,
            platform_override: "default".to_string(),
            dark_brightness: false,
        }
    }
}

impl OverlayState {
    pub const PLATFORMS: [&'static str; 5] = ["iOS", "android", "macOS", "linux", "windows"];

    pub fn is_enabled(&self, overlay: ServiceOverlay) -> bool {
        match overlay {
            ServiceOverlay::Inspector => self.inspector,
            ServiceOverlay::Performance => self.performance,
            ServiceOverlay::DebugPaint => self.debug_paint,
            ServiceOverlay::Baselines => self.baselines,
            ServiceOverlay::RepaintRainbow => self.repaint_rainbow,
            ServiceOverlay::SlowAnimations => self.slow_animations,
            ServiceOverlay::DebugBanner => self.debug_banner,
            ServiceOverlay::OversizedImages => self.oversized_images,
            ServiceOverlay::PlatformOverride => self.platform_override != "default",
            ServiceOverlay::BrightnessOverride => self.dark_brightness,
        }
    }

    pub fn badge_text(&self, overlay: ServiceOverlay) -> String {
        match overlay {
            ServiceOverlay::PlatformOverride => self.platform_override.clone(),
            ServiceOverlay::BrightnessOverride => {
                if self.dark_brightness {
                    "Dark".to_string()
                } else {
                    "Light".to_string()
                }
            }
            other => {
                if self.is_enabled(other) {
                    "ON".to_string()
                } else {
                    "OFF".to_string()
                }
            }
        }
    }

    /// Computes `(service_extension_method, params, status_label)` for toggling `overlay`
    /// without mutating `self`. Commit via `toggle(overlay)` only after the RPC succeeds.
    pub fn preview_toggle(&self, overlay: ServiceOverlay) -> (&'static str, Value, String) {
        let mut preview = self.clone();
        preview.toggle(overlay)
    }

    /// Flips the requested overlay and returns `(service_extension_method, params, status_label)`.
    pub fn toggle(&mut self, overlay: ServiceOverlay) -> (&'static str, Value, String) {
        let method = overlay.extension_method();
        match overlay {
            ServiceOverlay::Inspector => {
                self.inspector = !self.inspector;
                let v = self.inspector;
                (
                    method,
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::Performance => {
                self.performance = !self.performance;
                let v = self.performance;
                (
                    method,
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::DebugPaint => {
                self.debug_paint = !self.debug_paint;
                let v = self.debug_paint;
                (
                    method,
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::Baselines => {
                self.baselines = !self.baselines;
                let v = self.baselines;
                (
                    method,
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::RepaintRainbow => {
                self.repaint_rainbow = !self.repaint_rainbow;
                let v = self.repaint_rainbow;
                (
                    method,
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::SlowAnimations => {
                self.slow_animations = !self.slow_animations;
                let v = self.slow_animations;
                (
                    method,
                    json!({ "timeDilation": if v { "5.0" } else { "1.0" } }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::DebugBanner => {
                self.debug_banner = !self.debug_banner;
                let v = self.debug_banner;
                (
                    method,
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::OversizedImages => {
                self.oversized_images = !self.oversized_images;
                let v = self.oversized_images;
                (
                    method,
                    json!({ "enabled": v.to_string() }),
                    format!("{}: {}", overlay.label(), if v { "ON" } else { "OFF" }),
                )
            }
            ServiceOverlay::PlatformOverride => {
                let idx = self.platform_index % Self::PLATFORMS.len();
                let platform = Self::PLATFORMS[idx];
                self.platform_index = (idx + 1) % Self::PLATFORMS.len();
                self.platform_override = platform.to_string();
                (
                    method,
                    json!({ "value": platform }),
                    format!("{}: {platform}", overlay.label()),
                )
            }
            ServiceOverlay::BrightnessOverride => {
                self.dark_brightness = !self.dark_brightness;
                let value = if self.dark_brightness {
                    "Brightness.dark"
                } else {
                    "Brightness.light"
                };
                let display = if self.dark_brightness {
                    "Dark"
                } else {
                    "Light"
                };
                (
                    method,
                    json!({ "value": value }),
                    format!("{}: {display}", overlay.label()),
                )
            }
        }
    }

    pub fn sync_from_extension_event(&mut self, extension: &str, value: &str) {
        let truthy = value.eq_ignore_ascii_case("true");
        match extension {
            "ext.flutter.inspector.show" => self.inspector = truthy,
            "ext.flutter.showPerformanceOverlay" => self.performance = truthy,
            "ext.flutter.debugPaint" => self.debug_paint = truthy,
            "ext.flutter.debugPaintBaselinesEnabled" => self.baselines = truthy,
            "ext.flutter.repaintRainbow" => self.repaint_rainbow = truthy,
            "ext.flutter.timeDilation" => {
                self.slow_animations = value.parse::<f64>().is_ok_and(|v| v > 1.1);
            }
            "ext.flutter.debugAllowBanner" => self.debug_banner = truthy,
            "ext.flutter.invertOversizedImages" => self.oversized_images = truthy,
            "ext.flutter.platformOverride" => {
                if !value.is_empty() {
                    self.platform_override = value.to_string();
                    if let Some(pos) = Self::PLATFORMS.iter().position(|p| *p == value) {
                        self.platform_index = (pos + 1) % Self::PLATFORMS.len();
                    }
                }
            }
            "ext.flutter.brightnessOverride" => {
                self.dark_brightness = value.contains("dark");
            }
            _ => {}
        }
    }

    /// Synchronizes state from a human-readable IPC toggle status message (e.g. `"Target Platform: android"`).
    pub fn sync_from_status_message(&mut self, overlay: ServiceOverlay, message: &str) {
        let Some((_, rhs)) = message.split_once(':') else {
            return;
        };
        let val = rhs.trim();
        match overlay {
            ServiceOverlay::PlatformOverride => {
                self.sync_from_extension_event(overlay.extension_method(), val);
            }
            ServiceOverlay::BrightnessOverride => {
                let ext_val = if val.eq_ignore_ascii_case("Dark") {
                    "Brightness.dark"
                } else {
                    "Brightness.light"
                };
                self.sync_from_extension_event(overlay.extension_method(), ext_val);
            }
            other => {
                let ext_val = if val.eq_ignore_ascii_case("ON") {
                    "true"
                } else {
                    "false"
                };
                self.sync_from_extension_event(other.extension_method(), ext_val);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_state_toggles_and_syncs_all_extensions() {
        let mut state = OverlayState::default();
        let (method, params, label) = state.toggle(ServiceOverlay::Inspector);
        assert_eq!(method, "ext.flutter.inspector.show");
        assert_eq!(params["enabled"], "true");
        assert!(label.contains("ON"));

        let (method, params, label) = state.toggle(ServiceOverlay::BrightnessOverride);
        assert_eq!(method, "ext.flutter.brightnessOverride");
        assert_eq!(params["value"], "Brightness.dark");
        assert!(label.contains("Dark"));

        state.sync_from_extension_event("ext.flutter.platformOverride", "macOS");
        assert_eq!(state.platform_override, "macOS");
        assert_eq!(state.badge_text(ServiceOverlay::PlatformOverride), "macOS");
    }

    #[test]
    fn overlay_state_preview_does_not_mutate_until_committed() {
        let mut state = OverlayState::default();
        assert!(!state.inspector);

        let (method, params, label) = state.preview_toggle(ServiceOverlay::Inspector);
        assert_eq!(method, "ext.flutter.inspector.show");
        assert_eq!(params["enabled"], "true");
        assert!(label.contains("ON"));
        assert!(!state.inspector);

        let (m2, p2, l2) = state.toggle(ServiceOverlay::Inspector);
        assert_eq!(m2, method);
        assert_eq!(p2, params);
        assert_eq!(l2, label);
        assert!(state.inspector);
    }
}
