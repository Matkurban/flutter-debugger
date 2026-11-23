use std::collections::VecDeque;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        Selectable as _, Sizable as _,
        button::Button,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::backend::{FrameTiming, ServiceOverlay};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;

const MAX_FRAMES: usize = 120;

pub struct PerformanceTabState {
    pub frames: VecDeque<FrameTiming>,
    pub recording: bool,
    pub target_fps_120: bool,
    pub selected_frame_number: Option<u64>,
}

impl PerformanceTabState {
    pub fn new() -> Self {
        Self {
            frames: VecDeque::with_capacity(MAX_FRAMES),
            recording: true,
            target_fps_120: false,
            selected_frame_number: None,
        }
    }

    pub fn budget_ms(&self) -> f64 {
        if self.target_fps_120 { 8.33 } else { 16.67 }
    }

    pub fn push_frame(&mut self, frame: FrameTiming) {
        if !self.recording {
            return;
        }
        if self.frames.len() >= MAX_FRAMES {
            self.frames.pop_front();
        }
        if self.selected_frame_number.is_none() {
            self.selected_frame_number = Some(frame.number);
        }
        self.frames.push_back(frame);
    }

    pub fn jank_count(&self) -> usize {
        let budget = self.budget_ms();
        self.frames.iter().filter(|f| f.is_jank(budget)).count()
    }

    pub fn avg_ui_ms(&self) -> f64 {
        if self.frames.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.frames.iter().map(|f| f.build_ms()).sum();
        sum / (self.frames.len() as f64)
    }

    pub fn avg_raster_ms(&self) -> f64 {
        if self.frames.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.frames.iter().map(|f| f.raster_ms()).sum();
        sum / (self.frames.len() as f64)
    }

    pub fn p95_total_ms(&self) -> f64 {
        if self.frames.is_empty() {
            return 0.0;
        }
        let mut totals: Vec<f64> = self.frames.iter().map(|f| f.total_ms()).collect();
        totals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((totals.len() as f64) * 0.95).floor() as usize;
        totals[idx.min(totals.len() - 1)]
    }

    pub fn selected_frame(&self) -> Option<FrameTiming> {
        if let Some(num) = self.selected_frame_number
            && let Some(found) = self.frames.iter().find(|f| f.number == num)
        {
            return Some(*found);
        }
        self.frames.back().copied()
    }
}

impl RootView {
    pub fn render_performance_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let recording = self.performance.recording;
        let is_120 = self.performance.target_fps_120;
        let budget_ms = self.performance.budget_ms();
        let perf_overlay_on = self.overlays.performance;
        let repaint_on = self.overlays.repaint_rainbow;

        let avg_ui = self.performance.avg_ui_ms();
        let avg_raster = self.performance.avg_raster_ms();
        let p95_total = self.performance.p95_total_ms();
        let jank_count = self.performance.jank_count();
        let total_frames = self.performance.frames.len();
        let selected_frame = self.performance.selected_frame();
        let frames_snapshot: Vec<FrameTiming> = self.performance.frames.iter().copied().collect();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(DevToolsTheme::bg_main())
            // Sub-toolbar
            .child(
                div()
                    .w_full()
                    .h(px(36.0))
                    .px_3()
                    .bg(DevToolsTheme::bg_panel())
                    .border_b_1()
                    .border_color(DevToolsTheme::border())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                Button::new("perf-record-btn")
                                    .small()
                                    .compact()
                                    .selected(recording)
                                    .label(if recording {
                                        "● Recording"
                                    } else {
                                        "⏸ Paused"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.performance.recording = !this.performance.recording;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("perf-clear-btn")
                                    .small()
                                    .compact()
                                    .label("Clear")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.performance.frames.clear();
                                        this.performance.selected_frame_number = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("perf-60fps-btn")
                                    .small()
                                    .compact()
                                    .selected(!is_120)
                                    .label("60 FPS (16.6ms)")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.performance.target_fps_120 = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("perf-120fps-btn")
                                    .small()
                                    .compact()
                                    .selected(is_120)
                                    .label("120 FPS (8.3ms)")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.performance.target_fps_120 = true;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                Button::new("perf-overlay-toggle")
                                    .small()
                                    .compact()
                                    .selected(perf_overlay_on)
                                    .label("Performance Overlay")
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.toggle_overlay(ServiceOverlay::Performance);
                                    })),
                            )
                            .child(
                                Button::new("perf-repaint-toggle")
                                    .small()
                                    .compact()
                                    .selected(repaint_on)
                                    .label("Repaint Rainbow")
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.toggle_overlay(ServiceOverlay::RepaintRainbow);
                                    })),
                            ),
                    ),
            )
            // Body
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    // Top 4 Metric Cards
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(perf_metric_card(
                                "Avg UI (Build)",
                                &format!("{avg_ui:.2} ms"),
                                format!("Budget: {budget_ms:.1} ms"),
                                if avg_ui > budget_ms {
                                    DevToolsTheme::error_red()
                                } else {
                                    DevToolsTheme::accent_blue()
                                },
                            ))
                            .child(perf_metric_card(
                                "Avg Raster",
                                &format!("{avg_raster:.2} ms"),
                                "GPU Rasterizer Thread".to_string(),
                                if avg_raster > budget_ms {
                                    DevToolsTheme::error_red()
                                } else {
                                    DevToolsTheme::accent_purple()
                                },
                            ))
                            .child(perf_metric_card(
                                "P95 Total Frame",
                                &format!("{p95_total:.2} ms"),
                                "95th Percentile Latency".to_string(),
                                if p95_total > budget_ms * 1.5 {
                                    DevToolsTheme::warning_yellow()
                                } else {
                                    DevToolsTheme::success_green()
                                },
                            ))
                            .child(perf_metric_card(
                                "Jank Frames",
                                &format!("{jank_count} / {total_frames}"),
                                if total_frames > 0 {
                                    format!(
                                        "{:.1}% dropped frames",
                                        (jank_count as f64) * 100.0 / (total_frames as f64)
                                    )
                                } else {
                                    "Waiting for Flutter.Frame events".to_string()
                                },
                                if jank_count > 0 {
                                    DevToolsTheme::error_red()
                                } else {
                                    DevToolsTheme::success_green()
                                },
                            )),
                    )
                    // Native GPUI Dual-Color Frame Bar Chart
                    .child(render_frame_bar_chart(
                        frames_snapshot,
                        budget_ms,
                        selected_frame.map(|f| f.number),
                        cx,
                    ))
                    // Selected Frame Breakdown Card
                    .child(render_selected_frame_card(selected_frame, budget_ms)),
            )
    }
}

fn perf_metric_card(
    title: &str,
    value: &str,
    subtitle: String,
    accent: gpui_kit::Rgba,
) -> impl IntoElement + use<> {
    div()
        .flex_1()
        .p_3()
        .rounded(px(6.0))
        .bg(DevToolsTheme::bg_panel())
        .border_1()
        .border_color(DevToolsTheme::border())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(DevToolsTheme::text_secondary())
                .child(title.to_string()),
        )
        .child(
            div()
                .font_family(DevToolsTheme::MONO_FONT)
                .text_lg()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .text_color(accent)
                .child(value.to_string()),
        )
        .child(
            div()
                .font_family(DevToolsTheme::MONO_FONT)
                .text_xs()
                .text_color(DevToolsTheme::text_muted())
                .child(subtitle),
        )
}

fn render_frame_bar_chart(
    frames: Vec<FrameTiming>,
    budget_ms: f64,
    selected_num: Option<u64>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let chart_height_px: f32 = 240.0;
    let max_scale_ms = (budget_ms * 2.2).max(
        frames
            .iter()
            .map(|f| f.build_ms() + f.raster_ms())
            .fold(budget_ms * 2.0, f64::max),
    );
    let budget_line_bottom_px =
        ((budget_ms / max_scale_ms) as f32 * chart_height_px).clamp(12.0, chart_height_px - 12.0);

    div()
        .flex_1()
        .min_h(px(280.0))
        .w_full()
        .p_3()
        .rounded(px(6.0))
        .bg(DevToolsTheme::bg_panel())
        .border_1()
        .border_color(DevToolsTheme::border())
        .flex()
        .flex_col()
        .justify_between()
        // Header + Legend
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(DevToolsTheme::text_primary())
                        .child("Flutter.Frame Timeline (Last 120 Frames — Click any bar to inspect)"),
                )
                .child(
                    div()
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(legend_item(DevToolsTheme::accent_blue(), "UI (Build)"))
                        .child(legend_item(DevToolsTheme::accent_purple(), "Raster"))
                        .child(legend_item(DevToolsTheme::error_red(), "Jank (> Budget)")),
                ),
        )
        // Bars Container with Budget Reference Line
        .child(
            div()
                .relative()
                .w_full()
                .h(px(chart_height_px))
                .mt_2()
                .bg(DevToolsTheme::bg_main())
                .rounded(px(4.0))
                .border_1()
                .border_color(DevToolsTheme::border())
                .px_2()
                .flex()
                .items_end()
                .gap(px(2.0))
                // Budget Reference Line
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(budget_line_bottom_px))
                        .h(px(1.0))
                        .bg(DevToolsTheme::to_hsla(DevToolsTheme::error_red(), 0.7))
                        .flex()
                        .items_center()
                        .justify_end()
                        .pr_2()
                        .child(
                            div()
                                .px_1()
                                .rounded(px(2.0))
                                .bg(DevToolsTheme::bg_panel())
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_xs()
                                .text_color(DevToolsTheme::error_red())
                                .child(format!("Budget {budget_ms:.1}ms")),
                        ),
                )
                .when(frames.is_empty(), |el| {
                    el.child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(DevToolsTheme::text_muted())
                            .child("Interact with your Flutter app to stream Flutter.Frame timings..."),
                    )
                })
                .children(frames.into_iter().map(|frame| {
                    let is_jank = frame.is_jank(budget_ms);
                    let is_selected = selected_num == Some(frame.number);
                    let ui_h = ((frame.build_ms() / max_scale_ms) as f32 * chart_height_px)
                        .clamp(2.0, chart_height_px * 0.92);
                    let raster_h = ((frame.raster_ms() / max_scale_ms) as f32 * chart_height_px)
                        .clamp(2.0, chart_height_px - ui_h);
                    let frame_num = frame.number;

                    let ui_color = if is_jank && frame.build_ms() > budget_ms {
                        DevToolsTheme::error_red()
                    } else {
                        DevToolsTheme::accent_blue()
                    };
                    let raster_color = if is_jank && frame.raster_ms() > budget_ms {
                        DevToolsTheme::error_red()
                    } else {
                        DevToolsTheme::accent_purple()
                    };

                    div()
                        .id(SharedString::from(format!("frame-bar-{frame_num}")))
                        .flex_1()
                        .max_w(px(14.0))
                        .h_full()
                        .cursor_pointer()
                        .flex()
                        .flex_col()
                        .justify_end()
                        .when(is_selected, |el| {
                            el.bg(DevToolsTheme::to_hsla(DevToolsTheme::accent_blue(), 0.14))
                        })
                        .hover(|h| h.bg(DevToolsTheme::bg_hover()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.performance.selected_frame_number = Some(frame_num);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .w_full()
                                .h(px(raster_h))
                                .rounded_t(px(2.0))
                                .bg(raster_color),
                        )
                        .child(div().w_full().h(px(ui_h)).bg(ui_color))
                })),
        )
}

fn legend_item(color: gpui_kit::Rgba, label: &str) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().w(px(10.0)).h(px(10.0)).rounded(px(2.0)).bg(color))
        .child(
            div()
                .text_color(DevToolsTheme::text_secondary())
                .child(label.to_string()),
        )
}

fn render_selected_frame_card(selected: Option<FrameTiming>, budget_ms: f64) -> impl IntoElement {
    let Some(frame) = selected else {
        return div()
            .w_full()
            .p_3()
            .rounded(px(6.0))
            .bg(DevToolsTheme::bg_panel())
            .border_1()
            .border_color(DevToolsTheme::border())
            .font_family(DevToolsTheme::MONO_FONT)
            .text_xs()
            .text_color(DevToolsTheme::text_muted())
            .child("No frame selected");
    };

    let jank = frame.is_jank(budget_ms);
    let badge_color = if jank {
        DevToolsTheme::error_red()
    } else {
        DevToolsTheme::success_green()
    };

    div()
        .w_full()
        .p_3()
        .rounded(px(6.0))
        .bg(DevToolsTheme::bg_panel())
        .border_1()
        .border_color(DevToolsTheme::border())
        .font_family(DevToolsTheme::MONO_FONT)
        .text_xs()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::BOLD)
                        .text_color(DevToolsTheme::text_primary())
                        .child(format!("Frame #{}", frame.number)),
                )
                .child(
                    div()
                        .px_1p5()
                        .py(px(1.0))
                        .rounded(px(3.0))
                        .bg(DevToolsTheme::to_hsla(badge_color, 0.18))
                        .text_color(badge_color)
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(if jank { "JANK" } else { "SMOOTH" }),
                )
                .child(
                    div()
                        .text_color(DevToolsTheme::accent_blue())
                        .child(format!("UI Build: {:.2} ms", frame.build_ms())),
                )
                .child(
                    div()
                        .text_color(DevToolsTheme::accent_purple())
                        .child(format!("Raster: {:.2} ms", frame.raster_ms())),
                )
                .child(
                    div()
                        .text_color(DevToolsTheme::text_primary())
                        .child(format!("Total Elapsed: {:.2} ms", frame.total_ms())),
                )
                .child(
                    div()
                        .text_color(DevToolsTheme::text_secondary())
                        .child(format!(
                            "Vsync Overhead: {:.2} ms",
                            (frame.vsync_overhead_micros as f64) / 1000.0
                        )),
                ),
        )
        .child(
            div()
                .text_color(DevToolsTheme::text_muted())
                .child(format!("Target Budget: {budget_ms:.2} ms")),
        )
}
