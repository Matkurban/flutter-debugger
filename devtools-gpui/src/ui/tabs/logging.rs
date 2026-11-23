use std::collections::VecDeque;
use std::ops::Range;

use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, UniformListScrollHandle, Window,
    assets::IconName,
    base::SelectableText,
    component::{
        Icon, Selectable as _, Sizable as _,
        button::Button,
        h_resizable,
        input::{Input, InputState},
        resizable_panel,
        scroll::ScrollableElement as _,
    },
    div, point,
    prelude::FluentBuilder as _,
    px, uniform_list,
};

use crate::protocol::{LogEntry, LogKind, extract_source_locations_from_text};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;
use crate::ui::components::{empty_placeholder, pill_badge, subtoolbar_container};

const MAX_LOG_ENTRIES: usize = 10_000;
const LOG_ROW_HEIGHT_PX: f32 = 28.0;

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
    pub auto_scroll: bool,
    pub pending_new_count: usize,
    pub filter_chip: LogFilterChip,
    pub selected_seq: Option<u64>,
    pub pinned_selected_entry: Option<LogEntry>,
    pub search_input: Entity<InputState>,
    pub search_query: String,
    pub filtered_indices: Vec<usize>,
    pub scroll_handle: UniformListScrollHandle,
}

impl LoggingTabState {
    pub fn new(search_input: Entity<InputState>) -> Self {
        Self {
            entries: VecDeque::with_capacity(512),
            paused: false,
            auto_scroll: true,
            pending_new_count: 0,
            filter_chip: LogFilterChip::All,
            selected_seq: None,
            pinned_selected_entry: None,
            search_input,
            search_query: String::new(),
            filtered_indices: Vec::new(),
            scroll_handle: UniformListScrollHandle::new(),
        }
    }

    pub fn set_search_query(&mut self, query: String) {
        if self.search_query != query {
            self.search_query = query;
            self.rebuild_filtered_indices();
        }
    }

    pub fn set_filter_chip(&mut self, chip: LogFilterChip) {
        if self.filter_chip != chip {
            self.filter_chip = chip;
            self.rebuild_filtered_indices();
        }
    }

    /// Selects a log entry (or deselects if clicking the already-selected entry).
    /// Selecting an entry snapshots `pinned_selected_entry` and disables `auto_scroll`
    /// so incoming logs never push the inspected row away.
    pub fn select_entry(&mut self, seq: u64) {
        if self.selected_seq == Some(seq) {
            self.selected_seq = None;
            self.pinned_selected_entry = None;
        } else {
            self.selected_seq = Some(seq);
            self.pinned_selected_entry = self.entries.iter().find(|e| e.seq == seq).cloned();
            self.auto_scroll = false;
        }
    }

    pub fn unpin_selection(&mut self) {
        self.selected_seq = None;
        self.pinned_selected_entry = None;
    }

    pub fn jump_to_latest(&mut self) {
        self.selected_seq = None;
        self.pinned_selected_entry = None;
        self.auto_scroll = true;
        self.pending_new_count = 0;
        self.scroll_handle.scroll_to_bottom();
    }

    pub fn locate_selected(&mut self) {
        let Some(seq) = self.selected_seq else {
            return;
        };
        if let Some(row_ix) = self
            .filtered_indices
            .iter()
            .position(|&entry_idx| self.entries.get(entry_idx).is_some_and(|e| e.seq == seq))
        {
            self.scroll_handle
                .scroll_to_item(row_ix, gpui_kit::ScrollStrategy::Center);
        }
    }

    pub fn push_entry(&mut self, entry: LogEntry) {
        if self.paused {
            return;
        }
        let matches = self.entry_matches(&entry);

        if self.entries.len() >= MAX_LOG_ENTRIES {
            let evicted_visible = self.filtered_indices.first() == Some(&0);
            self.entries.pop_front();
            if evicted_visible {
                self.filtered_indices.remove(0);
            }
            for idx in &mut self.filtered_indices {
                *idx -= 1;
            }
            let new_idx = self.entries.len();
            self.entries.push_back(entry);
            if matches {
                self.filtered_indices.push(new_idx);
            }
            // Viewport anchoring: if a visible top entry was evicted while the user is reading/pinned,
            // shift the scroll offset by one row height (+28px towards 0) so the visible rows stay stationary.
            if evicted_visible && (!self.auto_scroll || self.selected_seq.is_some()) {
                let base_handle = &self.scroll_handle.0.borrow().base_handle;
                let cur = base_handle.offset();
                base_handle.set_offset(point(
                    cur.x,
                    (cur.y + px(LOG_ROW_HEIGHT_PX)).min(px(0.0)),
                ));
            }
        } else {
            let idx = self.entries.len();
            self.entries.push_back(entry);
            if matches {
                self.filtered_indices.push(idx);
            }
        }

        if self.auto_scroll && self.selected_seq.is_none() {
            self.scroll_handle.scroll_to_bottom();
        } else if matches {
            self.pending_new_count += 1;
        }
    }

    pub fn update_entry(&mut self, updated: LogEntry) {
        if self.selected_seq == Some(updated.seq) {
            self.pinned_selected_entry = Some(updated.clone());
        }
        if let Some(existing) = self.entries.iter_mut().find(|e| e.seq == updated.seq) {
            *existing = updated;
            self.rebuild_filtered_indices();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.filtered_indices.clear();
        self.selected_seq = None;
        self.pinned_selected_entry = None;
        self.pending_new_count = 0;
    }

    pub fn selected_entry(&self) -> Option<&LogEntry> {
        let seq = self.selected_seq?;
        self.entries.iter().find(|e| e.seq == seq).or_else(|| {
            self.pinned_selected_entry
                .as_ref()
                .filter(|e| e.seq == seq)
        })
    }

    fn entry_matches(&self, e: &LogEntry) -> bool {
        if !self.filter_chip.matches(e.kind) {
            return false;
        }
        let q = self.search_query.trim().to_ascii_lowercase();
        q.is_empty()
            || e.full_text.to_ascii_lowercase().contains(&q)
            || e.logger_name.to_ascii_lowercase().contains(&q)
            || e.stack_trace
                .as_deref()
                .is_some_and(|st| st.to_ascii_lowercase().contains(&q))
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
                    || e.stack_trace
                        .as_deref()
                        .is_some_and(|st| st.to_ascii_lowercase().contains(&q))
            })
            .map(|(idx, _)| idx)
            .collect();
    }
}

impl RootView {
    pub fn render_logging_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let paused = self.logging.paused;
        let auto_scroll = self.logging.auto_scroll && self.logging.selected_seq.is_none();
        let has_selection = self.logging.selected_seq.is_some();
        let pending_new = self.logging.pending_new_count;
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
                                    .icon(Icon::new(if paused {
                                        IconName::Play
                                    } else {
                                        IconName::Pause
                                    }))
                                    .label(if paused { "Resume" } else { "Live" })
                                    .tooltip("Pause or resume live log stream capture")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.logging.paused = !this.logging.paused;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("log-follow-btn")
                                    .small()
                                    .compact()
                                    .selected(auto_scroll)
                                    .icon(Icon::new(IconName::ArrowDownToLine))
                                    .label("Follow")
                                    .tooltip("Automatically scroll to newest log entry")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.logging.auto_scroll
                                            && this.logging.selected_seq.is_none()
                                        {
                                            this.logging.auto_scroll = false;
                                        } else {
                                            this.logging.jump_to_latest();
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("log-clear-btn")
                                    .small()
                                    .compact()
                                    .icon(Icon::new(IconName::Trash))
                                    .label("Clear")
                                    .tooltip("Clear log buffer")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.logging.clear();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div().w(px(220.0)).child(
                                    Input::new(&self.logging.search_input)
                                        .id("log-search-input")
                                        .small()
                                        .cleanable(true),
                                ),
                            )
                            .children(LogFilterChip::all().into_iter().map(|chip| {
                                Button::new(SharedString::from(format!(
                                    "log-chip-{}",
                                    chip.label()
                                )))
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
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .when(pending_new > 0, |el| {
                                el.child(
                                    Button::new("log-pending-new-btn")
                                        .small()
                                        .compact()
                                        .selected(true)
                                        .icon(Icon::new(IconName::ArrowDown))
                                        .label(format!("+{pending_new} new"))
                                        .tooltip("Jump to latest logs and resume auto-follow")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.logging.jump_to_latest();
                                            cx.notify();
                                        })),
                                )
                            })
                            .when(has_selection, |el| {
                                el.child(
                                    Button::new("log-locate-selected-btn")
                                        .small()
                                        .compact()
                                        .icon(Icon::new(IconName::LocateFixed))
                                        .label("Locate")
                                        .tooltip("Scroll selected log entry into center of view")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.logging.locate_selected();
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("log-unpin-selected-btn")
                                        .small()
                                        .compact()
                                        .label("Unpin")
                                        .tooltip("Deselect log entry")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.logging.unpin_selection();
                                            cx.notify();
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .font_family(DevToolsTheme::MONO_FONT)
                                    .text_xs()
                                    .text_color(DevToolsTheme::text_secondary())
                                    .child(format!(
                                        "{filtered_count} / {total_count} logs (max 10,000)"
                                    )),
                            ),
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
        .h(px(LOG_ROW_HEIGHT_PX))
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
            this.logging.select_entry(seq);
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
                .child(pill_badge(entry.kind.badge_label(), badge_color, 0.16)),
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
                    .flex_shrink_0()
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

fn render_log_detail_pane(
    selected: Option<LogEntry>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let Some(entry) = selected else {
        return empty_placeholder("Select a log entry to inspect full message and stack trace")
            .into_any_element();
    };
    let copy_text = match &entry.stack_trace {
        Some(st) => format!("{}\n\n{}", entry.full_text, st),
        None => entry.full_text.clone(),
    };
    let badge_color = log_kind_color(entry.kind);

    div()
        .id("log-detail-scroll")
        .relative()
        .size_full()
        .min_h_0()
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
                    Button::new("btn-copy-log-entry")
                        .small()
                        .compact()
                        .icon(Icon::new(IconName::Copy))
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
                .child(SelectableText::new(
                    SharedString::from(format!("log-full-text-{}", entry.seq)),
                    entry.full_text.clone(),
                )),
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
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(DevToolsTheme::accent_blue())
                            .child("Jump to Source in Zed"),
                    )
                    .children(
                        entry
                            .source_refs
                            .into_iter()
                            .enumerate()
                            .map(|(idx, loc)| {
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
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.jump_to_zed(loc_click.clone())
                                    }))
                                    .child(Icon::new(IconName::ExternalLink).xsmall())
                                    .child(format!("{}:{}:{}", loc.uri, loc.line, loc.column))
                            }),
                    ),
            )
        })
        .when_some(entry.stack_trace, |el, stack| {
            let seq = entry.seq;
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
                            .text_color(DevToolsTheme::error_red())
                            .child("Stack Trace"),
                    )
                    .child(
                        div()
                            .font_family(DevToolsTheme::MONO_FONT)
                            .text_xs()
                            .text_color(DevToolsTheme::text_primary())
                            .child(SelectableText::new(
                                SharedString::from(format!("log-stack-sel-{seq}")),
                                stack.clone(),
                            )),
                    )
                    .children(stack.lines().enumerate().filter_map(|(line_idx, line)| {
                        let loc = extract_source_locations_from_text(line).into_iter().next()?;
                        let line_str = line.trim().to_string();
                        Some(
                            div()
                                .id(SharedString::from(format!("stack-line-{seq}-{line_idx}")))
                                .py(px(1.5))
                                .px_1p5()
                                .rounded(px(3.0))
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_xs()
                                .cursor_pointer()
                                .text_color(DevToolsTheme::accent_blue())
                                .hover(|h| h.bg(DevToolsTheme::bg_selected()))
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .on_click(cx.listener(move |this, _, _, _| {
                                    this.jump_to_zed(loc.clone())
                                }))
                                .child(Icon::new(IconName::ExternalLink).xsmall())
                                .child(line_str),
                        )
                    })),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure unit test helper exercising `LoggingTabState` selection pinning, `auto_scroll` freezing,
    /// `pending_new_count` tracking, and incremental ring-buffer eviction without needing a GPUI window.
    #[test]
    fn selecting_log_disables_auto_scroll_tracks_pending_and_survives_ring_buffer_eviction() {
        let mut entries: VecDeque<LogEntry> = VecDeque::with_capacity(MAX_LOG_ENTRIES + 4);
        let mut filtered_indices: Vec<usize> = Vec::with_capacity(MAX_LOG_ENTRIES + 4);
        let mut pending_new_count = 0usize;

        // 1. Push initial entries 1..=MAX_LOG_ENTRIES
        for seq in 1..=(MAX_LOG_ENTRIES as u64) {
            let e = LogEntry::from_text(
                seq,
                LogKind::Stdout,
                "stdout",
                format!("log line {seq}"),
                None,
            );
            let idx = entries.len();
            entries.push_back(e);
            filtered_indices.push(idx);
        }
        assert_eq!(entries.len(), MAX_LOG_ENTRIES);
        assert_eq!(filtered_indices.len(), MAX_LOG_ENTRIES);

        // 2. Select seq=1 (the very first entry at index 0) -> disables auto_scroll & pins entry
        let selected_seq = Some(1u64);
        let pinned_selected_entry = entries.iter().find(|e| e.seq == 1).cloned();
        let auto_scroll = false;
        assert!(!auto_scroll);
        assert!(pinned_selected_entry.is_some());

        // 3. Push 2 more matching entries -> evicts seq=1 and seq=2 from ring buffer,
        //    increments pending_new_count, shifts filtered_indices incrementally,
        //    and preserves selected entry detail via pinned_selected_entry!
        for seq in (MAX_LOG_ENTRIES as u64 + 1)..=(MAX_LOG_ENTRIES as u64 + 2) {
            let entry = LogEntry::from_text(
                seq,
                LogKind::Stdout,
                "stdout",
                format!("overflow log line {seq}"),
                None,
            );
            let evicted_visible = filtered_indices.first() == Some(&0);
            assert!(evicted_visible);
            entries.pop_front();
            if evicted_visible {
                filtered_indices.remove(0);
            }
            for idx in &mut filtered_indices {
                *idx -= 1;
            }
            let new_idx = entries.len();
            entries.push_back(entry);
            filtered_indices.push(new_idx);
            if !(auto_scroll && selected_seq.is_none()) {
                pending_new_count += 1;
            }
        }

        assert_eq!(entries.len(), MAX_LOG_ENTRIES);
        assert_eq!(entries.front().unwrap().seq, 3);
        assert_eq!(entries.back().unwrap().seq, MAX_LOG_ENTRIES as u64 + 2);
        assert_eq!(filtered_indices.len(), MAX_LOG_ENTRIES);
        assert_eq!(filtered_indices.first(), Some(&0));
        assert_eq!(filtered_indices.last(), Some(&(MAX_LOG_ENTRIES - 1)));
        assert_eq!(pending_new_count, 2);

        // Even though seq=1 was evicted from `entries`, the pinned snapshot keeps the detail pane alive!
        let resolved_selected = entries
            .iter()
            .find(|e| Some(e.seq) == selected_seq)
            .or_else(|| {
                pinned_selected_entry
                    .as_ref()
                    .filter(|e| Some(e.seq) == selected_seq)
            });
        assert_eq!(
            resolved_selected.map(|e| e.full_text.as_str()),
            Some("log line 1")
        );
    }
}
