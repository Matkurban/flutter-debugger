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

use crate::backend::{
    BackendCommand, HttpRequestDetail, HttpRequestRecord, JsonTokenKind, format_and_tokenize_json,
};
use crate::theme::{DevToolsTheme, format_bytes, format_duration_ms};
use crate::ui::app::RootView;

const MAX_NETWORK_REQUESTS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkFilterChip {
    All,
    Status2xx,
    Status4xx5xx,
    Pending,
    Json,
}

impl NetworkFilterChip {
    pub const fn all() -> [Self; 5] {
        [
            Self::All,
            Self::Status2xx,
            Self::Status4xx5xx,
            Self::Pending,
            Self::Json,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Status2xx => "2xx",
            Self::Status4xx5xx => "4xx/5xx",
            Self::Pending => "Pending",
            Self::Json => "JSON",
        }
    }

    pub fn matches(self, rec: &HttpRequestRecord) -> bool {
        match self {
            Self::All => true,
            Self::Status2xx => matches!(rec.status_code, Some(200..=299)),
            Self::Status4xx5xx => {
                matches!(rec.status_code, Some(400..=599)) || rec.error.is_some()
            }
            Self::Pending => rec.is_pending(),
            Self::Json => rec.is_json,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkSortColumn {
    Timestamp,
    Method,
    Address,
    Status,
    Type,
    Duration,
    Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkDetailSubTab {
    Overview,
    Headers,
    Request,
    Response,
}

impl NetworkDetailSubTab {
    pub const fn all() -> [Self; 4] {
        [
            Self::Overview,
            Self::Headers,
            Self::Request,
            Self::Response,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Headers => "Headers",
            Self::Request => "Request",
            Self::Response => "Response",
        }
    }
}

pub struct NetworkTabState {
    pub requests: VecDeque<HttpRequestRecord>,
    pub recording: bool,
    pub filter_chip: NetworkFilterChip,
    pub sort_column: NetworkSortColumn,
    pub sort_ascending: bool,
    pub selected_id: Option<String>,
    pub detail_tab: NetworkDetailSubTab,
    pub search_input: Entity<InputState>,
}

impl NetworkTabState {
    pub fn new(search_input: Entity<InputState>) -> Self {
        Self {
            requests: VecDeque::with_capacity(256),
            recording: true,
            filter_chip: NetworkFilterChip::All,
            sort_column: NetworkSortColumn::Timestamp,
            sort_ascending: false,
            selected_id: None,
            detail_tab: NetworkDetailSubTab::Response,
            search_input,
        }
    }

    /// Merges an incremental batch of HTTP profile requests, deduplicating by `id`
    /// and preserving any previously fetched request/response bodies.
    /// Returns `Some(selected_id)` if the currently selected request was updated without details.
    pub fn merge_batch(&mut self, batch: Vec<HttpRequestRecord>) -> Option<String> {
        let mut refetch_selected = None;
        for incoming in batch {
            if let Some(existing) = self.requests.iter_mut().find(|r| r.id == incoming.id) {
                let prev_detail = existing.detail.take();
                *existing = incoming;
                if existing.detail.is_none() {
                    existing.detail = prev_detail;
                }
                if self.selected_id.as_deref() == Some(existing.id.as_str())
                    && existing.detail.is_none()
                    && !existing.is_pending()
                {
                    refetch_selected = Some(existing.id.clone());
                }
            } else {
                if self.requests.len() >= MAX_NETWORK_REQUESTS {
                    self.requests.pop_front();
                }
                self.requests.push_back(incoming);
            }
        }
        refetch_selected
    }

    pub fn apply_detail(&mut self, request_id: &str, detail: HttpRequestDetail) {
        if let Some(rec) = self.requests.iter_mut().find(|r| r.id == request_id) {
            if rec.request_bytes == 0
                && let Some(ref b) = detail.request_body
            {
                rec.request_bytes = b.len() as u64;
            }
            if rec.response_bytes == 0
                && let Some(ref b) = detail.response_body
            {
                rec.response_bytes = b.len() as u64;
            }
            rec.detail = Some(detail);
        }
    }

    pub fn selected_record(&self) -> Option<&HttpRequestRecord> {
        let id = self.selected_id.as_deref()?;
        self.requests.iter().find(|r| r.id == id)
    }

    pub fn toggle_sort(&mut self, col: NetworkSortColumn) {
        if self.sort_column == col {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort_column = col;
            self.sort_ascending = true;
        }
    }

    pub fn filtered_and_sorted(&self, query: &str) -> Vec<HttpRequestRecord> {
        let q = query.trim().to_ascii_lowercase();
        let mut list: Vec<HttpRequestRecord> = self
            .requests
            .iter()
            .filter(|rec| self.filter_chip.matches(rec))
            .filter(|rec| {
                if q.is_empty() {
                    return true;
                }
                rec.uri.to_ascii_lowercase().contains(&q)
                    || rec.method.to_ascii_lowercase().contains(&q)
                    || rec
                        .status_code
                        .is_some_and(|s| s.to_string().contains(&q))
                    || rec.mime_short.to_ascii_lowercase().contains(&q)
            })
            .cloned()
            .collect();

        list.sort_by(|a, b| {
            let ord = match self.sort_column {
                NetworkSortColumn::Timestamp => a.start_time_micros.cmp(&b.start_time_micros),
                NetworkSortColumn::Method => a.method.cmp(&b.method),
                NetworkSortColumn::Address => a.path_and_query.cmp(&b.path_and_query),
                NetworkSortColumn::Status => {
                    a.status_code.unwrap_or(0).cmp(&b.status_code.unwrap_or(0))
                }
                NetworkSortColumn::Type => a.mime_short.cmp(&b.mime_short),
                NetworkSortColumn::Duration => {
                    a.duration_ms.unwrap_or(0).cmp(&b.duration_ms.unwrap_or(0))
                }
                NetworkSortColumn::Size => a.total_bytes().cmp(&b.total_bytes()),
            };
            if self.sort_ascending {
                ord
            } else {
                ord.reverse()
            }
        });
        list
    }
}

impl RootView {
    pub fn render_network_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.network.search_input.read(cx).value().to_string();
        let rows = self.network.filtered_and_sorted(&query);
        let selected_record = self.network.selected_record().cloned();
        let recording = self.network.recording;
        let current_chip = self.network.filter_chip;
        let sort_col = self.network.sort_column;
        let sort_asc = self.network.sort_ascending;
        let detail_tab = self.network.detail_tab;

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
                                Button::new("net-record-btn")
                                    .small()
                                    .compact()
                                    .selected(recording)
                                    .label(if recording {
                                        "● Recording"
                                    } else {
                                        "⏸ Paused"
                                    })
                                    .tooltip("Toggle HTTP traffic capture")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.network.recording = !this.network.recording;
                                        this.send_cmd(BackendCommand::SetNetworkRecording(
                                            this.network.recording,
                                        ));
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("net-clear-btn")
                                    .small()
                                    .compact()
                                    .label("Clear")
                                    .tooltip("Clear captured HTTP requests")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.network.requests.clear();
                                        this.network.selected_id = None;
                                        this.send_cmd(BackendCommand::ClearNetworkProfile);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(240.0))
                                    .child(
                                        Input::new(&self.network.search_input)
                                            .id("net-search-input")
                                            .small()
                                            .cleanable(true),
                                    ),
                            )
                            .children(NetworkFilterChip::all().into_iter().map(|chip| {
                                let active = chip == current_chip;
                                Button::new(SharedString::from(format!(
                                    "net-chip-{}",
                                    chip.label()
                                )))
                                .small()
                                .compact()
                                .selected(active)
                                .label(chip.label())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.network.filter_chip = chip;
                                    cx.notify();
                                }))
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .font_family(DevToolsTheme::MONO_FONT)
                                    .text_xs()
                                    .text_color(DevToolsTheme::text_secondary())
                                    .child(format!(
                                        "{} / {} requests",
                                        rows.len(),
                                        self.network.requests.len()
                                    )),
                            )
                            .child(
                                Button::new("net-copy-curl-btn")
                                    .small()
                                    .compact()
                                    .label("Copy as cURL")
                                    .tooltip("Copy selected HTTP request as a cURL command")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(rec) = this.network.selected_record() {
                                            let curl = rec.to_curl_command();
                                            this.copy_to_clipboard(
                                                curl,
                                                "Copied request as cURL command",
                                                window,
                                                cx,
                                            );
                                        } else {
                                            this.show_toast(
                                                "Select a request first to copy as cURL",
                                                true,
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                    ),
            )
            // Left / Right resizable split workspace
            .child(
                div().flex_1().min_h_0().w_full().child(
                    h_resizable("network-split-pane")
                        .child(
                            resizable_panel()
                                .size(px(680.0))
                                .size_range(px(380.0)..px(1100.0))
                                .child(render_requests_table(
                                    rows,
                                    self.network.selected_id.as_deref(),
                                    sort_col,
                                    sort_asc,
                                    cx,
                                )),
                        )
                        .child(resizable_panel().child(render_request_detail_pane(
                            selected_record,
                            detail_tab,
                            cx,
                        ))),
                ),
            )
    }
}

fn render_requests_table(
    rows: Vec<HttpRequestRecord>,
    selected_id: Option<&str>,
    sort_col: NetworkSortColumn,
    sort_asc: bool,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let sort_arrow = |col: NetworkSortColumn| -> &'static str {
        if sort_col == col {
            if sort_asc { " ▲" } else { " ▼" }
        } else {
            ""
        }
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(DevToolsTheme::bg_main())
        // Table header
        .child(
            div()
                .w_full()
                .h(px(28.0))
                .px_2()
                .bg(DevToolsTheme::bg_toolbar())
                .border_b_1()
                .border_color(DevToolsTheme::border())
                .font_family(DevToolsTheme::MONO_FONT)
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(DevToolsTheme::text_secondary())
                .flex()
                .items_center()
                .child(
                    div()
                        .id("col-method")
                        .w(px(64.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.network.toggle_sort(NetworkSortColumn::Method);
                            cx.notify();
                        }))
                        .child(format!("Method{}", sort_arrow(NetworkSortColumn::Method))),
                )
                .child(
                    div()
                        .id("col-address")
                        .flex_1()
                        .min_w(px(140.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.network.toggle_sort(NetworkSortColumn::Address);
                            cx.notify();
                        }))
                        .child(format!("Address{}", sort_arrow(NetworkSortColumn::Address))),
                )
                .child(
                    div()
                        .id("col-status")
                        .w(px(68.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.network.toggle_sort(NetworkSortColumn::Status);
                            cx.notify();
                        }))
                        .child(format!("Status{}", sort_arrow(NetworkSortColumn::Status))),
                )
                .child(
                    div()
                        .id("col-type")
                        .w(px(56.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.network.toggle_sort(NetworkSortColumn::Type);
                            cx.notify();
                        }))
                        .child(format!("Type{}", sort_arrow(NetworkSortColumn::Type))),
                )
                .child(
                    div()
                        .id("col-duration")
                        .w(px(82.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.network.toggle_sort(NetworkSortColumn::Duration);
                            cx.notify();
                        }))
                        .child(format!(
                            "Duration{}",
                            sort_arrow(NetworkSortColumn::Duration)
                        )),
                )
                .child(
                    div()
                        .id("col-size")
                        .w(px(76.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.network.toggle_sort(NetworkSortColumn::Size);
                            cx.notify();
                        }))
                        .child(format!("Size{}", sort_arrow(NetworkSortColumn::Size))),
                )
                .child(
                    div()
                        .id("col-time")
                        .w(px(92.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.network.toggle_sort(NetworkSortColumn::Timestamp);
                            cx.notify();
                        }))
                        .child(format!("Time{}", sort_arrow(NetworkSortColumn::Timestamp))),
                ),
        )
        // Table body
        .child(
            div()
                .id("network-rows-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .flex_col()
                .overflow_y_scrollbar()
                .when(rows.is_empty(), |el| {
                    el.child(
                        div()
                            .w_full()
                            .py_12()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(DevToolsTheme::text_muted())
                            .child("No HTTP requests matching current filter."),
                    )
                })
                .children(rows.into_iter().map(|rec| {
                    let is_selected = selected_id == Some(rec.id.as_str());
                    let req_id = rec.id.clone();
                    let method_color = DevToolsTheme::method_color(&rec.method);
                    let status_color = DevToolsTheme::status_color(rec.status_code);
                    let status_label = match rec.status_code {
                        Some(code) => code.to_string(),
                        None if rec.error.is_some() => "ERR".to_string(),
                        None => "...".to_string(),
                    };

                    div()
                        .id(SharedString::from(format!("net-row-{}", rec.id)))
                        .w_full()
                        .h(px(28.0))
                        .px_2()
                        .border_b_1()
                        .border_color(DevToolsTheme::border())
                        .cursor_pointer()
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .flex()
                        .items_center()
                        .when(is_selected, |el| el.bg(DevToolsTheme::bg_selected()))
                        .when(!is_selected, |el| {
                            el.hover(|h| h.bg(DevToolsTheme::bg_hover()))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.network.selected_id = Some(req_id.clone());
                            this.send_cmd(BackendCommand::FetchHttpRequestDetail {
                                request_id: req_id.clone(),
                            });
                            cx.notify();
                        }))
                        // Method badge
                        .child(
                            div().w(px(64.0)).flex().items_center().child(
                                div()
                                    .px_1p5()
                                    .py(px(1.0))
                                    .rounded(px(3.0))
                                    .bg(DevToolsTheme::to_hsla(method_color, 0.16))
                                    .text_color(method_color)
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(rec.method.clone()),
                            ),
                        )
                        // Address (path first, host muted)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(140.0))
                                .pr_2()
                                .overflow_hidden()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_color(DevToolsTheme::text_primary())
                                        .truncate()
                                        .child(rec.path_and_query.clone()),
                                )
                                .when(!rec.host.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .text_color(DevToolsTheme::text_muted())
                                            .truncate()
                                            .child(format!("({})", rec.host)),
                                    )
                                }),
                        )
                        // Status
                        .child(
                            div()
                                .w(px(68.0))
                                .text_color(status_color)
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(status_label),
                        )
                        // Type
                        .child(
                            div()
                                .w(px(56.0))
                                .text_color(DevToolsTheme::text_secondary())
                                .child(rec.mime_short.clone()),
                        )
                        // Duration
                        .child(
                            div()
                                .w(px(82.0))
                                .text_color(if rec.duration_ms.unwrap_or(0) > 1000 {
                                    DevToolsTheme::warning_yellow()
                                } else {
                                    DevToolsTheme::text_primary()
                                })
                                .child(format_duration_ms(rec.duration_ms)),
                        )
                        // Size
                        .child(
                            div()
                                .w(px(76.0))
                                .text_color(DevToolsTheme::text_secondary())
                                .child(format_bytes(rec.total_bytes())),
                        )
                        // Timestamp
                        .child(
                            div()
                                .w(px(92.0))
                                .text_color(DevToolsTheme::text_muted())
                                .child(rec.timestamp_hms.clone()),
                        )
                })),
        )
}

fn render_request_detail_pane(
    selected: Option<HttpRequestRecord>,
    active_sub_tab: NetworkDetailSubTab,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let Some(rec) = selected else {
        return div()
            .size_full()
            .bg(DevToolsTheme::bg_panel())
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(DevToolsTheme::text_muted())
            .child("No request selected");
    };

    let detail = rec.detail.clone().unwrap_or_default();

    div()
        .size_full()
        .bg(DevToolsTheme::bg_panel())
        .flex()
        .flex_col()
        // Sub-tab bar
        .child(
            div()
                .w_full()
                .h(px(32.0))
                .px_3()
                .bg(DevToolsTheme::bg_toolbar())
                .border_b_1()
                .border_color(DevToolsTheme::border())
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .h_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(NetworkDetailSubTab::all().into_iter().map(|tab| {
                            let is_active = tab == active_sub_tab;
                            div()
                                .id(SharedString::from(format!("net-detail-tab-{}", tab.label())))
                                .h_full()
                                .px_2p5()
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .border_b_2()
                                .text_xs()
                                .when(is_active, |el| {
                                    el.border_color(DevToolsTheme::accent_blue())
                                        .text_color(DevToolsTheme::text_primary())
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                })
                                .when(!is_active, |el| {
                                    el.border_color(gpui_kit::transparent_black())
                                        .text_color(DevToolsTheme::text_secondary())
                                        .hover(|h| h.text_color(DevToolsTheme::text_primary()))
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.network.detail_tab = tab;
                                    cx.notify();
                                }))
                                .child(tab.label())
                        })),
                )
                .child(
                    div()
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .text_color(DevToolsTheme::status_color(rec.status_code))
                        .child(format!(
                            "{} {}",
                            rec.method,
                            rec.status_code
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "Pending".into())
                        )),
                ),
        )
        // Sub-tab content
        .child(
            div()
                .id("net-detail-body-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .p_3()
                .overflow_y_scrollbar()
                .child(match active_sub_tab {
                    NetworkDetailSubTab::Overview => render_overview_subtab(&rec, &detail).into_any_element(),
                    NetworkDetailSubTab::Headers => render_headers_subtab(&detail).into_any_element(),
                    NetworkDetailSubTab::Request => render_body_subtab(
                        "Request Body",
                        detail.request_body.as_deref(),
                        cx,
                    )
                    .into_any_element(),
                    NetworkDetailSubTab::Response => render_body_subtab(
                        "Response Body",
                        detail.response_body.as_deref(),
                        cx,
                    )
                    .into_any_element(),
                }),
        )
}

fn render_overview_subtab(
    rec: &HttpRequestRecord,
    detail: &HttpRequestDetail,
) -> impl IntoElement + use<> {
    let status_text = match (rec.status_code, rec.reason_phrase.as_deref()) {
        (Some(code), Some(phrase)) => format!("{code} {phrase}"),
        (Some(code), None) => code.to_string(),
        (None, _) => "Pending".to_string(),
    };

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(
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
                        .child("General"),
                )
                .child(kv_row("Request URL", &rec.uri))
                .child(kv_row("Request Method", &rec.method))
                .child(kv_row("Status Code", &status_text))
                .child(kv_row(
                    "Content-Type",
                    rec.content_type.as_deref().unwrap_or("-"),
                ))
                .child(kv_row("Duration", &format_duration_ms(rec.duration_ms)))
                .child(kv_row("Request Size", &format_bytes(rec.request_bytes)))
                .child(kv_row("Response Size", &format_bytes(rec.response_bytes)))
                .when_some(detail.remote_address.as_deref(), |el, addr| {
                    el.child(kv_row("Remote Address", addr))
                })
                .when_some(rec.error.as_deref(), |el, err| {
                    el.child(kv_row("Error", err))
                }),
        )
        .child(
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
                        .text_color(DevToolsTheme::accent_cyan())
                        .child("Timing Breakdown"),
                )
                .child(kv_row("Started At", &rec.timestamp_hms))
                .child(kv_row("Total Elapsed", &format_duration_ms(rec.duration_ms)))
                .when(rec.events.is_empty(), |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(DevToolsTheme::text_muted())
                            .child("No granular timeline events recorded for this request."),
                    )
                })
                .children(rec.events.iter().map(|ev| {
                    let offset_ms = ev
                        .timestamp_micros
                        .saturating_sub(rec.start_time_micros)
                        / 1000;
                    kv_row(&ev.name, &format!("+{offset_ms} ms"))
                })),
        )
}

fn render_headers_subtab(detail: &HttpRequestDetail) -> impl IntoElement + use<> {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(
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
                        .child(format!(
                            "Response Headers ({})",
                            detail.response_headers.len()
                        )),
                )
                .when(detail.response_headers.is_empty(), |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(DevToolsTheme::text_muted())
                            .child("No response headers"),
                    )
                })
                .children(
                    detail
                        .response_headers
                        .iter()
                        .map(|(k, v)| kv_row(k, v)),
                ),
        )
        .child(
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
                        .text_color(DevToolsTheme::accent_purple())
                        .child(format!(
                            "Request Headers ({})",
                            detail.request_headers.len()
                        )),
                )
                .when(detail.request_headers.is_empty(), |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(DevToolsTheme::text_muted())
                            .child("No request headers"),
                    )
                })
                .children(detail.request_headers.iter().map(|(k, v)| kv_row(k, v))),
        )
}

fn render_body_subtab(
    title: &str,
    body_opt: Option<&str>,
    cx: &mut Context<RootView>,
) -> impl IntoElement + use<> {
    let Some(raw_body) = body_opt.filter(|s| !s.trim().is_empty()) else {
        return div()
            .w_full()
            .p_6()
            .rounded(px(6.0))
            .bg(DevToolsTheme::bg_main())
            .border_1()
            .border_color(DevToolsTheme::border())
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(DevToolsTheme::text_muted())
            .child(format!("No {title} payload available."));
    };

    let (pretty_text, highlighted_lines) = format_and_tokenize_json(raw_body);
    let copy_payload = pretty_text.clone();
    let title_owned = title.to_string();

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(DevToolsTheme::text_secondary())
                        .child(format!(
                            "{title} ({} lines · {})",
                            highlighted_lines.len(),
                            format_bytes(raw_body.len() as u64)
                        )),
                )
                .child(
                    Button::new(SharedString::from(format!("btn-copy-{title}")))
                        .small()
                        .compact()
                        .label("Copy Body")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.copy_to_clipboard(
                                copy_payload.clone(),
                                &format!("Copied {title_owned} to clipboard"),
                                window,
                                cx,
                            );
                        })),
                ),
        )
        .child(
            div()
                .w_full()
                .p_3()
                .rounded(px(6.0))
                .bg(DevToolsTheme::bg_main())
                .border_1()
                .border_color(DevToolsTheme::border())
                .font_family(DevToolsTheme::MONO_FONT)
                .text_xs()
                .flex()
                .flex_col()
                .gap_0p5()
                .children(highlighted_lines.into_iter().enumerate().map(|(idx, line)| {
                    div()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .w(px(32.0))
                                .text_color(DevToolsTheme::text_muted())
                                .child(format!("{:>3}", idx + 1)),
                        )
                        .child(
                            div().flex_1().flex().flex_wrap().children(
                                line.spans.into_iter().map(|(kind, text)| {
                                    let color = match kind {
                                        JsonTokenKind::Punctuation => {
                                            DevToolsTheme::text_secondary()
                                        }
                                        JsonTokenKind::Key => DevToolsTheme::accent_blue(),
                                        JsonTokenKind::StringVal => DevToolsTheme::success_green(),
                                        JsonTokenKind::NumberVal => DevToolsTheme::warning_yellow(),
                                        JsonTokenKind::BoolOrNull => DevToolsTheme::accent_purple(),
                                    };
                                    div().text_color(color).child(text)
                                }),
                            ),
                        )
                })),
        )
}

fn kv_row(key: &str, value: &str) -> impl IntoElement + use<> {
    div()
        .w_full()
        .font_family(DevToolsTheme::MONO_FONT)
        .text_xs()
        .flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .w(px(140.0))
                .flex_shrink_0()
                .text_color(DevToolsTheme::text_secondary())
                .child(format!("{key}:")),
        )
        .child(
            div()
                .flex_1()
                .text_color(DevToolsTheme::text_primary())
                .child(value.to_string()),
        )
}
