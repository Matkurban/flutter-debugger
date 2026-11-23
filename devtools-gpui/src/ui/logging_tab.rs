use std::collections::VecDeque;

use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        Selectable as _, Sizable as _,
        button::Button,
        h_resizable,
        input::{Input, InputState},
        resizable_panel,
        scroll::ScrollableElement as _,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::backend::{LogEntry, LogKind};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;

const MAX_LOG_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFilterChip {
    All,
    Error,
    Stdout,
    Log,
    Gc,
    Flutter,
}

impl LogFilterChip {
    pub const fn all() -> [Self; 6] {
        [
            Self::All,
            Self::Error,
            Self::Stdout,
            Self::Log,
            Self::Gc,
            Self::Flutter,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Error => "Error",
            Self::Stdout => "Stdout",
            Self::Log => "Log",
            Self::Gc => "GC",
            Self::Flutter => "Flutter",
        }
    }

    pub fn matches(self, kind: LogKind) -> bool {
        match self {
            Self::All => true,
            Self::Error => matches!(kind, LogKind::FlutterError | LogKind::Stderr),
            Self::Stdout => kind == LogKind::Stdout,
            Self::Log => kind == LogKind::Log,
            Self::Gc => kind == LogKind::Gc,
            Self::Flutter => matches!(kind, LogKind::FlutterError | LogKind::FlutterInfo),
        }
    }
}

pub struct LoggingTabState {
    pub entries: VecDeque<LogEntry>,
    pub paused: bool,
    pub filter_chip: LogFilterChip,
    pub selected_seq: Option<u64>,
    pub search_input: Entity<InputState>,
}

impl LoggingTabState {
    pub fn new(search_input: Entity<InputState>) -> Self {
        Self {
            entries: VecDeque::with_capacity(512),
            paused: false,
            filter_chip: LogFilterChip::All,
            selected_seq: None,
            search_input,
        }
    }

    pub fn push_entry(&mut self, entry: LogEntry) {
        if self.paused {
            return;
        }
        if self.entries.len() >= MAX_LOG_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    pub fn selected_entry(&self) -> Option<&LogEntry> {
        let seq = self.selected_seq?;
        self.entries.iter().find(|e| e.seq == seq)
    }

    pub fn filtered_entries(&self, query: &str) -> Vec<LogEntry> {
        let q = query.trim().to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| self.filter_chip.matches(e.kind))
            .filter(|e| {
                if q.is_empty() {
                    return true;
                }
                e.full_text.to_ascii_lowercase().contains(&q)
                    || e.logger_name.to_ascii_lowercase().contains(&q)
                    || e.stack_trace
                        .as_deref()
                        .is_some_and(|st| st.to_ascii_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }
}

impl RootView {
    pub fn render_logging_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.logging.search_input.read(cx).value().to_string();
        let filtered = self.logging.filtered_entries(&query);
        let selected_entry = self.logging.selected_entry().cloned();
        let paused = self.logging.paused;
        let current_chip = self.logging.filter_chip;

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
                                Button::new("log-pause-btn")
                                    .small()
                                    .compact()
                                    .selected(!paused)
                                    .label(if paused { "▶ Resume" } else { "⏸ Live" })
                                    .tooltip("Pause or resume live log stream capture")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.logging.paused = !this.logging.paused;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("log-clear-btn")
                                    .small()
                                    .compact()
                                    .label("Clear")
                                    .tooltip("Clear log buffer")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.logging.entries.clear();
                                        this.logging.selected_seq = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(240.0))
                                    .child(
                                        Input::new(&self.logging.search_input)
                                            .id("log-search-input")
                                            .small()
                                            .cleanable(true),
                                    ),
                            )
                            .children(LogFilterChip::all().into_iter().map(|chip| {
                                let active = chip == current_chip;
                                Button::new(SharedString::from(format!(
                                    "log-chip-{}",
                                    chip.label()
                                )))
                                .small()
                                .compact()
                                .selected(active)
                                .label(chip.label())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.logging.filter_chip = chip;
                                    cx.notify();
                                }))
                            })),
                    )
                    .child(
                        div()
                            .font_family(DevToolsTheme::MONO_FONT)
                            .text_xs()
                            .text_color(DevToolsTheme::text_secondary())
                            .child(format!(
                                "{} / {} logs (max 10,000)",
                                filtered.len(),
                                self.logging.entries.len()
                            )),
                    ),
            )
            // Split list + detail view
            .child(
                div().flex_1().min_h_0().w_full().child(
                    h_resizable("logging-split-pane")
                        .child(
                            resizable_panel()
                                .size(px(740.0))
                                .size_range(px(360.0)..px(1100.0))
                                .child(render_log_stream_list(
                                    filtered,
                                    self.logging.selected_seq,
                                    cx,
                                )),
                        )
                        .child(
                            resizable_panel()
                                .child(render_log_detail_pane(selected_entry, cx)),
                        ),
                ),
            )
    }
}

fn log_kind_color(kind: LogKind) -> gpui_kit::Rgba {
    match kind {
        LogKind::Stdout => DevToolsTheme::text_secondary(),
        LogKind::Stderr | LogKind::FlutterError => DevToolsTheme::error_red(),
        LogKind::Log => DevToolsTheme::accent_blue(),
        LogKind::FlutterInfo => DevToolsTheme::accent_cyan(),
        LogKind::Gc => DevToolsTheme::accent_purple(),
    }
}

fn render_log_stream_list(
    entries: Vec<LogEntry>,
    selected_seq: Option<u64>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    div()
        .id("logging-rows-scroll")
        .size_full()
        .bg(DevToolsTheme::bg_main())
        .flex()
        .flex_col()
        .overflow_y_scrollbar()
        .when(entries.is_empty(), |el| {
            el.child(
                div()
                    .w_full()
                    .py_12()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(DevToolsTheme::text_muted())
                    .child("No log events captured yet."),
            )
        })
        .children(entries.into_iter().map(|entry| {
            let is_selected = selected_seq == Some(entry.seq);
            let seq = entry.seq;
            let badge_color = log_kind_color(entry.kind);
            let first_loc = entry.source_refs.first().cloned();

            div()
                .id(SharedString::from(format!("log-row-{seq}")))
                .w_full()
                .h(px(28.0))
                .px_2p5()
                .border_b_1()
                .border_color(DevToolsTheme::border())
                .cursor_pointer()
                .font_family(DevToolsTheme::MONO_FONT)
                .text_xs()
                .flex()
                .items_center()
                .gap_2()
                .when(is_selected, |el| el.bg(DevToolsTheme::bg_selected()))
                .when(!is_selected, |el| {
                    el.hover(|h| h.bg(DevToolsTheme::bg_hover()))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.logging.selected_seq = Some(seq);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(64.0))
                        .flex_shrink_0()
                        .text_color(DevToolsTheme::text_muted())
                        .child(entry.timestamp_hms.clone()),
                )
                .child(
                    div()
                        .w(px(64.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .px_1p5()
                                .py(px(1.0))
                                .rounded(px(3.0))
                                .bg(DevToolsTheme::to_hsla(badge_color, 0.16))
                                .text_color(badge_color)
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(entry.kind.badge_label()),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(if entry.kind.is_error() {
                            DevToolsTheme::error_red()
                        } else {
                            DevToolsTheme::text_primary()
                        })
                        .truncate()
                        .child(entry.summary.clone()),
                )
                .when_some(first_loc, |el, loc| {
                    let loc_jump = loc.clone();
                    el.child(
                        div()
                            .id(SharedString::from(format!("log-jump-{seq}")))
                            .px_1p5()
                            .py(px(1.0))
                            .rounded(px(3.0))
                            .bg(DevToolsTheme::bg_elevated())
                            .text_color(DevToolsTheme::accent_blue())
                            .hover(|h| h.bg(DevToolsTheme::bg_selected()))
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.jump_to_zed(loc_jump.clone());
                            }))
                            .child(loc.short_label()),
                    )
                })
        }))
}

fn render_log_detail_pane(
    selected: Option<LogEntry>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let Some(entry) = selected else {
        return div()
            .size_full()
            .bg(DevToolsTheme::bg_panel())
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(DevToolsTheme::text_muted())
            .child("Select a log entry to inspect full message and stack trace")
            .into_any_element();
    };

    let copy_text = match &entry.stack_trace {
        Some(st) => format!("{}\n\n{}", entry.full_text, st),
        None => entry.full_text.clone(),
    };
    let badge_color = log_kind_color(entry.kind);

    div()
        .id("log-detail-scroll")
        .size_full()
        .bg(DevToolsTheme::bg_panel())
        .p_3()
        .flex()
        .flex_col()
        .gap_3()
        .overflow_y_scrollbar()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .px_1p5()
                                .py(px(1.0))
                                .rounded(px(3.0))
                                .bg(DevToolsTheme::to_hsla(badge_color, 0.2))
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(badge_color)
                                .child(entry.kind.badge_label()),
                        )
                        .child(
                            div()
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_xs()
                                .text_color(DevToolsTheme::text_secondary())
                                .child(format!(
                                    "{} · {}",
                                    entry.logger_name, entry.timestamp_hms
                                )),
                        ),
                )
                .child(
                    Button::new("btn-copy-log-entry")
                        .small()
                        .compact()
                        .label("Copy Log")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.copy_to_clipboard(
                                copy_text.clone(),
                                "Copied log entry to clipboard",
                                window,
                                cx,
                            );
                        })),
                ),
        )
        // Message box
        .child(
            div()
                .p_3()
                .rounded(px(6.0))
                .bg(DevToolsTheme::bg_main())
                .border_1()
                .border_color(DevToolsTheme::border())
                .font_family(DevToolsTheme::MONO_FONT)
                .text_xs()
                .text_color(DevToolsTheme::text_primary())
                .child(entry.full_text.clone()),
        )
        // Source links extracted from message/stack
        .when(!entry.source_refs.is_empty(), |el| {
            el.child(
                div()
                    .p_3()
                    .rounded(px(6.0))
                    .bg(DevToolsTheme::bg_main())
                    .border_1()
                    .border_color(DevToolsTheme::border())
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(DevToolsTheme::accent_blue())
                            .child("Jump to Source in Zed"),
                    )
                    .children(entry.source_refs.into_iter().enumerate().map(|(idx, loc)| {
                        let loc_click = loc.clone();
                        div()
                            .id(SharedString::from(format!("log-detail-src-{idx}")))
                            .px_2()
                            .py_1()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .bg(DevToolsTheme::bg_panel())
                            .hover(|h| h.bg(DevToolsTheme::bg_selected()))
                            .font_family(DevToolsTheme::MONO_FONT)
                            .text_xs()
                            .text_color(DevToolsTheme::accent_cyan())
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.jump_to_zed(loc_click.clone());
                            }))
                            .child(format!("↗ {}:{}:{}", loc.uri, loc.line, loc.column))
                    })),
            )
        })
        // Stack trace
        .when_some(entry.stack_trace, |el, stack| {
            el.child(
                div()
                    .p_3()
                    .rounded(px(6.0))
                    .bg(DevToolsTheme::bg_main())
                    .border_1()
                    .border_color(DevToolsTheme::border())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(DevToolsTheme::error_red())
                            .child("Stack Trace"),
                    )
                    .children(stack.lines().enumerate().map(|(line_idx, line)| {
                        let line_locs =
                            crate::backend::models::extract_source_locations_from_text(line);
                        let first_loc = line_locs.into_iter().next();
                        let line_str = line.to_string();

                        div()
                            .id(SharedString::from(format!("stack-line-{line_idx}")))
                            .py(px(1.5))
                            .px_1()
                            .rounded(px(3.0))
                            .font_family(DevToolsTheme::MONO_FONT)
                            .text_xs()
                            .when_some(first_loc, |row, loc| {
                                row.cursor_pointer()
                                    .text_color(DevToolsTheme::accent_blue())
                                    .hover(|h| h.bg(DevToolsTheme::bg_selected()))
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.jump_to_zed(loc.clone());
                                    }))
                            })
                            .child(line_str)
                    })),
            )
        })
        .into_any_element()
}
