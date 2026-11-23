use std::collections::VecDeque;

use gpui_kit::{
    Bounds, ContentMask, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    PathBuilder, Pixels, Point, Styled as _, Window,
    assets::IconName,
    canvas,
    component::{
        Icon, Selectable as _, Sizable as _,
        button::Button,
        scroll::ScrollableElement as _,
    },
    div, point, px,
};

use crate::backend::BackendCommand;
use crate::protocol::MemorySnapshot;
use crate::theme::{DevToolsTheme, format_bytes};
use crate::ui::app::RootView;
use crate::ui::components::{legend_swatch, metric_card, subtoolbar_container};

const MAX_MEMORY_SAMPLES: usize = 180;

pub struct MemoryTabState {
    pub snapshots: VecDeque<MemorySnapshot>,
    pub polling: bool,
}

impl MemoryTabState {
    pub fn new() -> Self {
        Self {
            snapshots: VecDeque::with_capacity(MAX_MEMORY_SAMPLES),
            polling: true,
        }
    }

    pub fn push_snapshot(&mut self, snap: MemorySnapshot) {
        if self.snapshots.len() >= MAX_MEMORY_SAMPLES {
            self.snapshots.pop_front();
        }
        self.snapshots.push_back(snap);
    }
}

impl RootView {
    pub fn render_memory_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let polling = self.memory.polling;
        let latest = self.memory.snapshots.back().cloned();
        let samples: Vec<MemorySnapshot> = self.memory.snapshots.iter().cloned().collect();
        let gc_count = samples.iter().filter(|s| s.is_gc_event).count();

        let (heap_used, heap_cap, ext_used, rss_total, rss_subtitle) = match &latest {
            Some(s) => (
                format_bytes(s.heap_usage_bytes),
                format_bytes(s.heap_capacity_bytes),
                format_bytes(s.external_usage_bytes),
                format_bytes(s.rss_bytes),
                if s.rss_is_estimated {
                    "Estimated (Heap Capacity + External)"
                } else {
                    "Process Resident Set Size (getProcessMemoryUsage)"
                },
            ),
            None => (
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "Process Resident Set Size",
            ),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(DevToolsTheme::bg_main())
            .child(
                subtoolbar_container()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                Button::new("mem-poll-btn")
                                    .small()
                                    .compact()
                                    .selected(polling)
                                    .icon(Icon::new(if polling {
                                        IconName::CircleDot
                                    } else {
                                        IconName::Pause
                                    }))
                                    .label(if polling {
                                        "Live Polling (1s)"
                                    } else {
                                        "Paused"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.memory.polling = !this.memory.polling;
                                        this.send_cmd(BackendCommand::SetMemoryPolling(
                                            this.memory.polling,
                                        ));
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("mem-gc-btn")
                                    .small()
                                    .compact()
                                    .icon(Icon::new(IconName::Trash))
                                    .label("Force GC")
                                    .tooltip("Trigger full Dart VM Garbage Collection (getAllocationProfile gc: true)")
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.send_cmd(BackendCommand::ForceGc);
                                    })),
                            )
                            .child(
                                Button::new("mem-clear-btn")
                                    .small()
                                    .compact()
                                    .icon(Icon::new(IconName::Ban))
                                    .label("Clear")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.memory.snapshots.clear();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .font_family(DevToolsTheme::MONO_FONT)
                            .text_xs()
                            .text_color(DevToolsTheme::text_secondary())
                            .child(format!(
                                "{} / 180s samples · {} GC events",
                                samples.len(),
                                gc_count
                            )),
                    ),
            )
            .child(
                div()
                    .id("memory-main-scroll")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(metric_card(
                                "Heap Used",
                                &heap_used,
                                "Active Dart Objects",
                                DevToolsTheme::accent_blue(),
                            ))
                            .child(metric_card(
                                "Heap Capacity",
                                &heap_cap,
                                "Allocated Isolate Heap",
                                DevToolsTheme::accent_cyan(),
                            ))
                            .child(metric_card(
                                "External",
                                &ext_used,
                                "Native / TypedData Buffers",
                                DevToolsTheme::accent_purple(),
                            ))
                            .child(metric_card(
                                "RSS / Total",
                                &rss_total,
                                rss_subtitle,
                                DevToolsTheme::success_green(),
                            )),
                    )
                    .child(render_memory_area_chart(samples)),
            )
    }
}

fn render_memory_area_chart(samples: Vec<MemorySnapshot>) -> impl IntoElement {
    let max_bytes = samples
        .iter()
        .map(|s| {
            s.heap_capacity_bytes
                .max(s.heap_usage_bytes + s.external_usage_bytes)
        })
        .max()
        .unwrap_or(64 * 1024 * 1024)
        .max(16 * 1024 * 1024);

    let top_label = format_bytes(max_bytes);
    let mid_label = format_bytes(max_bytes / 2);

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
        .gap_2()
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
                        .child("Heap Memory Usage Trend (180s Rolling Window)"),
                )
                .child(
                    div()
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(legend_swatch(DevToolsTheme::accent_blue(), "Heap Used"))
                        .child(legend_swatch(DevToolsTheme::accent_cyan(), "Heap Capacity"))
                        .child(legend_swatch(DevToolsTheme::accent_purple(), "External"))
                        .child(legend_swatch(DevToolsTheme::warning_yellow(), "GC Event")),
                ),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .rounded(px(4.0))
                .bg(DevToolsTheme::bg_main())
                .border_1()
                .border_color(DevToolsTheme::border())
                .overflow_hidden()
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds: Bounds<Pixels>, _, window, _| {
                            paint_memory_canvas(bounds, &samples, max_bytes, window);
                        },
                    )
                    .size_full(),
                )
                .child(
                    div()
                        .absolute()
                        .top_1()
                        .left_2()
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .text_color(DevToolsTheme::text_muted())
                        .child(top_label),
                )
                .child(
                    div()
                        .absolute()
                        .top_1_2()
                        .left_2()
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .text_color(DevToolsTheme::text_muted())
                        .child(mid_label),
                ),
        )
}

fn paint_memory_canvas(
    bounds: Bounds<Pixels>,
    samples: &[MemorySnapshot],
    max_bytes: u64,
    window: &mut Window,
) {
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    if width <= 4.0 || height <= 4.0 || samples.is_empty() {
        return;
    }

    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let bottom = top + height;
    let max_f = (max_bytes as f32).max(1.0);

    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let grid_color = DevToolsTheme::to_hsla(DevToolsTheme::border(), 0.7);
        for frac in [0.25f32, 0.5, 0.75] {
            let y = top + height * frac;
            let mut grid_line = PathBuilder::stroke(px(1.0));
            grid_line.move_to(point(px(left), px(y)));
            grid_line.line_to(point(px(left + width), px(y)));
            if let Ok(path) = grid_line.build() {
                window.paint_path(path, grid_color);
            }
        }

        let n = samples.len();
        let x_for = |idx: usize| -> f32 {
            if n <= 1 {
                left + width
            } else {
                left + (idx as f32) / ((n - 1) as f32) * width
            }
        };
        let y_for = |bytes: u64| -> f32 {
            let ratio = ((bytes as f32) / max_f).clamp(0.0, 0.98);
            bottom - ratio * height
        };

        let cap_points: Vec<Point<Pixels>> = samples
            .iter()
            .enumerate()
            .map(|(i, s)| point(px(x_for(i)), px(y_for(s.heap_capacity_bytes))))
            .collect();
        paint_line_series(
            &cap_points,
            DevToolsTheme::to_hsla(DevToolsTheme::accent_cyan(), 0.75),
            window,
        );

        let used_points: Vec<Point<Pixels>> = samples
            .iter()
            .enumerate()
            .map(|(i, s)| point(px(x_for(i)), px(y_for(s.heap_usage_bytes))))
            .collect();
        paint_area_series(
            &used_points,
            bottom,
            DevToolsTheme::to_hsla(DevToolsTheme::accent_blue(), 0.22),
            window,
        );
        paint_line_series(&used_points, DevToolsTheme::accent_blue().into(), window);

        let ext_points: Vec<Point<Pixels>> = samples
            .iter()
            .enumerate()
            .map(|(i, s)| point(px(x_for(i)), px(y_for(s.external_usage_bytes))))
            .collect();
        paint_line_series(&ext_points, DevToolsTheme::accent_purple().into(), window);

        let gc_color = DevToolsTheme::to_hsla(DevToolsTheme::warning_yellow(), 0.85);
        for (i, s) in samples.iter().enumerate() {
            if s.is_gc_event {
                let x = x_for(i);
                let mut gc_line = PathBuilder::stroke(px(1.5));
                gc_line.move_to(point(px(x), px(top)));
                gc_line.line_to(point(px(x), px(bottom)));
                if let Ok(path) = gc_line.build() {
                    window.paint_path(path, gc_color);
                }
            }
        }
    });
}

fn paint_area_series(
    pts: &[Point<Pixels>],
    bottom_y: f32,
    fill_color: gpui_kit::Hsla,
    window: &mut Window,
) {
    if pts.len() < 2 {
        return;
    }
    let mut area = PathBuilder::fill();
    area.move_to(point(pts[0].x, px(bottom_y)));
    for pt in pts {
        area.line_to(*pt);
    }
    if let Some(last) = pts.last() {
        area.line_to(point(last.x, px(bottom_y)));
    }
    area.close();
    if let Ok(path) = area.build() {
        window.paint_path(path, fill_color);
    }
}

fn paint_line_series(
    pts: &[Point<Pixels>],
    stroke_color: gpui_kit::Hsla,
    window: &mut Window,
) {
    if pts.is_empty() {
        return;
    }
    let mut line = PathBuilder::stroke(px(2.0));
    line.move_to(pts[0]);
    if pts.len() == 1 {
        line.line_to(point(pts[0].x + px(2.0), pts[0].y));
    } else {
        for pt in &pts[1..] {
            line.line_to(*pt);
        }
    }
    if let Ok(path) = line.build() {
        window.paint_path(path, stroke_color);
    }
}
