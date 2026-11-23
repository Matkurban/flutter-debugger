use std::collections::VecDeque;
use std::ops::Range;

use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, UniformListScrollHandle, Window,
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
    px, uniform_list,
};

use crate::protocol::{LogEntry, LogKind, extract_source_locations_from_text};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;
use crate::ui::components::{empty_placeholder, pill_badge, subtoolbar_container};

const MAX_LOG_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFilterChip { All, Error, Stdout, Log, Gc, Flutter }

impl LogFilterChip {
    pub const fn all() -> [Self; 6] { [Self::All, Self::Error, Self::Stdout, Self::Log, Self::Gc, Self::Flutter] }
    pub fn label(self) -> &'static str {
        match self { Self::All => "All", Self::Error => "Error", Self::Stdout => "Stdout", Self::Log => "Log", Self::Gc => "GC", Self::Flutter => "Flutter" }
    }
    pub fn matches(self, kind: LogKind) -> bool {
        match self {
            Self::All => true, Self::Error => matches!(kind, LogKind::FlutterError | LogKind::Stderr),
            Self::Stdout => kind == LogKind::Stdout, Self::Log => kind == LogKind::Log,
            Self::Gc => kind == LogKind::Gc, Self::Flutter => matches!(kind, LogKind::FlutterError | LogKind::FlutterInfo),
        }
    }
}

pub struct LoggingTabState {
    pub entries: VecDeque<LogEntry>,
    pub paused: bool,
    pub filter_chip: LogFilterChip,
    pub selected_seq: Option<u64>,
    pub search_input: Entity<InputState>,
    pub search_query: String,
    pub filtered_indices: Vec<usize>,
    pub scroll_handle: UniformListScrollHandle,
}

impl LoggingTabState {
    pub fn new(search_input: Entity<InputState>) -> Self {
        Self {
            entries: VecDeque::with_capacity(512), paused: false, filter_chip: LogFilterChip::All,
            selected_seq: None, search_input, search_query: String::new(),
            filtered_indices: Vec::new(), scroll_handle: UniformListScrollHandle::new(),
        }
    }
    pub fn set_search_query(&mut self, query: String) {
        if self.search_query != query { self.search_query = query; self.rebuild_filtered_indices(); }
    }
    pub fn set_filter_chip(&mut self, chip: LogFilterChip) {
        if self.filter_chip != chip { self.filter_chip = chip; self.rebuild_filtered_indices(); }
    }
    pub fn push_entry(&mut self, entry: LogEntry) {
        if self.paused { return; }
        if self.entries.len() >= MAX_LOG_ENTRIES {
            self.entries.pop_front();
            self.entries.push_back(entry);
            self.rebuild_filtered_indices();
            return;
        }
        let idx = self.entries.len();
        let matches = self.entry_matches(&entry);
        self.entries.push_back(entry);
        if matches { self.filtered_indices.push(idx); }
    }
    pub fn update_entry(&mut self, updated: LogEntry) {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.seq == updated.seq) {
            *existing = updated;
            self.rebuild_filtered_indices();
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.filtered_indices.clear();
        self.selected_seq = None;
    }
    pub fn selected_entry(&self) -> Option<&LogEntry> {
        let seq = self.selected_seq?;
        self.entries.iter().find(|e| e.seq == seq)
    }

    fn entry_matches(&self, e: &LogEntry) -> bool {
        if !self.filter_chip.matches(e.kind) {
            return false;
        }
        let q = self.search_query.trim().to_ascii_lowercase();
        q.is_empty()
            || e.full_text.to_ascii_lowercase().contains(&q)
            || e.logger_name.to_ascii_lowercase().contains(&q)
            || e.stack_trace.as_deref().is_some_and(|st| st.to_ascii_lowercase().contains(&q))
    }

    pub fn rebuild_filtered_indices(&mut self) {
        let q = self.search_query.trim().to_ascii_lowercase();
        let chip = self.filter_chip;
        self.filtered_indices = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| chip.matches(e.kind))
            .filter(|(_, e)| {
                q.is_empty()
                    || e.full_text.to_ascii_lowercase().contains(&q)
                    || e.logger_name.to_ascii_lowercase().contains(&q)
                    || e.stack_trace.as_deref().is_some_and(|st| st.to_ascii_lowercase().contains(&q))
            })
            .map(|(idx, _)| idx)
            .collect();
    }
}

impl RootView {
    pub fn render_logging_tab(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paused = self.logging.paused;
        let current_chip = self.logging.filter_chip;
        let filtered_count = self.logging.filtered_indices.len();
        let total_count = self.logging.entries.len();
        let selected_entry = self.logging.selected_entry().cloned();

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
                                        this.logging.clear();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div().w(px(240.0)).child(
                                    Input::new(&self.logging.search_input).id("log-search-input").small().cleanable(true),
                                ),
                            )
                            .children(LogFilterChip::all().into_iter().map(|chip| {
                                Button::new(SharedString::from(format!("log-chip-{}", chip.label())))
                                    .small()
                                    .compact()
                                    .selected(chip == current_chip)
                                    .label(chip.label())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.logging.set_filter_chip(chip);
                                        cx.notify();
                                    }))
                            })),
                    )
                    .child(
                        div()
                            .font_family(DevToolsTheme::MONO_FONT)
                            .text_xs()
                            .text_color(DevToolsTheme::text_secondary())
                            .child(format!("{filtered_count} / {total_count} logs (max 10,000)")),
                    ),
            )
            .child(
                div().flex_1().min_h_0().w_full().child(
                    h_resizable("logging-split-pane")
                        .child(
                            resizable_panel()
                                .size(px(740.0))
                                .size_range(px(360.0)..px(1100.0))
                                .child(render_virtual_log_list(self, cx)),
                        )
                        .child(resizable_panel().child(render_log_detail_pane(selected_entry, cx))),
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

fn render_virtual_log_list(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let row_count = view.logging.filtered_indices.len();
    let scroll_handle = view.logging.scroll_handle.clone();
    if row_count == 0 {
        return div()
            .size_full()
            .bg(DevToolsTheme::bg_main())
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(DevToolsTheme::text_muted())
            .child("No log events captured yet.")
            .into_any_element();
    }

    div()
        .relative()
        .size_full()
        .bg(DevToolsTheme::bg_main())
        .child(
            uniform_list(
                "logging-uniform-list",
                row_count,
                cx.processor(
                    move |this: &mut RootView,
                          visible_range: Range<usize>,
                          _window: &mut Window,
                          cx: &mut Context<RootView>| {
                        let selected_seq = this.logging.selected_seq;
                        let mut items = Vec::with_capacity(visible_range.len());
                        for row_ix in visible_range {
                            if let Some(&entry_idx) = this.logging.filtered_indices.get(row_ix)
                                && let Some(entry) = this.logging.entries.get(entry_idx)
                            {
                                let is_selected = selected_seq == Some(entry.seq);
                                items.push(render_single_log_row(entry, is_selected, cx));
                            }
                        }
                        items
                    },
                ),
            )
            .size_full()
            .track_scroll(&scroll_handle),
        )
        .vertical_scrollbar(&scroll_handle)
        .into_any_element()
}

fn render_single_log_row(
    entry: &LogEntry,
    is_selected: bool,
    cx: &mut Context<RootView>,
) -> impl IntoElement + use<> {
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
        .when(!is_selected, |el| el.hover(|h| h.bg(DevToolsTheme::bg_hover())))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.logging.selected_seq = Some(seq);
            cx.notify();
        }))
        .child(div().w(px(64.0)).flex_shrink_0().text_color(DevToolsTheme::text_muted()).child(entry.timestamp_hms.clone()))
        .child(div().w(px(64.0)).flex_shrink_0().flex().items_center().child(pill_badge(entry.kind.badge_label(), badge_color, 0.16)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(if entry.kind.is_error() { DevToolsTheme::error_red() } else { DevToolsTheme::text_primary() })
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.jump_to_zed(loc_jump.clone());
                    }))
                    .child(loc.short_label()),
            )
        })
}

fn render_log_detail_pane(selected: Option<LogEntry>, cx: &mut Context<RootView>) -> impl IntoElement {
    let Some(entry) = selected else {
        return empty_placeholder("Select a log entry to inspect full message and stack trace").into_any_element();
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
                        .child(pill_badge(entry.kind.badge_label(), badge_color, 0.2))
                        .child(
                            div()
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_xs()
                                .text_color(DevToolsTheme::text_secondary())
                                .child(format!("{} · {}", entry.logger_name, entry.timestamp_hms)),
                        ),
                )
                .child(
                    Button::new("btn-copy-log-entry").small().compact().label("Copy Log").on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.copy_to_clipboard(copy_text.clone(), "Copied log entry to clipboard", window, cx);
                        }),
                    ),
                ),
        )
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
                    .child(div().text_xs().font_weight(gpui_kit::FontWeight::SEMIBOLD).text_color(DevToolsTheme::accent_blue()).child("Jump to Source in Zed"))
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
                            .on_click(cx.listener(move |this, _, _, _| this.jump_to_zed(loc_click.clone())))
                            .child(format!("↗ {}:{}:{}", loc.uri, loc.line, loc.column))
                    })),
            )
        })
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
                    .child(div().text_xs().font_weight(gpui_kit::FontWeight::SEMIBOLD).text_color(DevToolsTheme::error_red()).child("Stack Trace"))
                    .children(stack.lines().enumerate().map(|(line_idx, line)| {
                        let first_loc = extract_source_locations_from_text(line).into_iter().next();
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
                                    .on_click(cx.listener(move |this, _, _, _| this.jump_to_zed(loc.clone())))
                            })
                            .child(line_str)
                    })),
            )
        })
        .into_any_element()
}
