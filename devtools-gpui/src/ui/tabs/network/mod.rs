pub mod detail;
pub mod table;

use std::collections::{HashMap, VecDeque};

use gpui_kit::{
    Context, Entity, IntoElement, ParentElement as _, SharedString, Styled as _,
    UniformListScrollHandle, Window,
    assets::IconName,
    component::{
        Icon, Selectable as _, Sizable as _,
        button::Button,
        h_resizable,
        input::{Input, InputState},
        resizable_panel,
    },
    div, px,
};

use crate::backend::BackendCommand;
use crate::protocol::{
    FormattedBody, HttpRequestDetail, HttpRequestRecord, format_and_tokenize_json,
};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;
use crate::ui::components::subtoolbar_container;

use detail::render_request_detail_pane;
use table::render_requests_table;

const MAX_NETWORK_REQUESTS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkFilterChip { All, Status2xx, Status4xx5xx, Pending, Json }

impl NetworkFilterChip {
    pub const fn all() -> [Self; 5] {
        [Self::All, Self::Status2xx, Self::Status4xx5xx, Self::Pending, Self::Json]
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All", Self::Status2xx => "2xx", Self::Status4xx5xx => "4xx/5xx",
            Self::Pending => "Pending", Self::Json => "JSON",
        }
    }
    pub fn matches(self, rec: &HttpRequestRecord) -> bool {
        match self {
            Self::All => true,
            Self::Status2xx => matches!(rec.status_code, Some(200..=299)),
            Self::Status4xx5xx => matches!(rec.status_code, Some(400..=599)) || rec.error.is_some(),
            Self::Pending => rec.is_pending(),
            Self::Json => rec.is_json,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkSortColumn { Timestamp, Method, Address, Status, Type, Duration, Size }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkDetailSubTab { Overview, Headers, Request, Response }

impl NetworkDetailSubTab {
    pub const fn all() -> [Self; 4] { [Self::Overview, Self::Headers, Self::Request, Self::Response] }
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview", Self::Headers => "Headers",
            Self::Request => "Request", Self::Response => "Response",
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
    pub search_query: String,
    pub filtered_indices: Vec<usize>,
    pub scroll_handle: UniformListScrollHandle,
    pub detail_loading_id: Option<String>,
    pub detail_errors: HashMap<String, String>,
    pub cached_detail_id: Option<String>,
    pub cached_req_body: Option<FormattedBody>,
    pub cached_resp_body: Option<FormattedBody>,
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
            search_query: String::new(),
            filtered_indices: Vec::new(),
            scroll_handle: UniformListScrollHandle::new(),
            detail_loading_id: None,
            detail_errors: HashMap::new(),
            cached_detail_id: None,
            cached_req_body: None,
            cached_resp_body: None,
        }
    }

    pub fn set_search_query(&mut self, query: String) {
        if self.search_query != query {
            self.search_query = query;
            self.rebuild_filtered_indices();
        }
    }

    pub fn set_filter_chip(&mut self, chip: NetworkFilterChip) {
        if self.filter_chip != chip {
            self.filter_chip = chip;
            self.rebuild_filtered_indices();
        }
    }

    pub fn toggle_sort(&mut self, col: NetworkSortColumn) {
        if self.sort_column == col {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort_column = col;
            self.sort_ascending = true;
        }
        self.rebuild_filtered_indices();
    }

    /// Merges an incremental batch of HTTP profile requests, deduplicating by `id`
    /// and preserving previously fetched request/response bodies.
    pub fn merge_batch(
        &mut self,
        batch: Vec<HttpRequestRecord>,
    ) -> Option<(String, Option<String>)> {
        let refetch_selected =
            merge_requests_into_deque(&mut self.requests, batch, self.selected_id.as_deref());
        self.rebuild_filtered_indices();
        if let Some((ref id, _)) = refetch_selected
            && self.selected_id.as_deref() == Some(id.as_str())
        {
            self.rebuild_selected_body_cache();
        }
        refetch_selected
    }

    pub fn mark_detail_loading(&mut self, request_id: &str) {
        self.detail_loading_id = Some(request_id.to_string());
        self.detail_errors.remove(request_id);
    }

    pub fn apply_detail_error(&mut self, request_id: &str, error: String) {
        if self.detail_loading_id.as_deref() == Some(request_id) {
            self.detail_loading_id = None;
        }
        self.detail_errors.insert(request_id.to_string(), error);
    }

    pub fn apply_detail(&mut self, request_id: &str, mut detail: HttpRequestDetail) {
        if self.detail_loading_id.as_deref() == Some(request_id) {
            self.detail_loading_id = None;
        }
        self.detail_errors.remove(request_id);

        if let Some(rec) = self.requests.iter_mut().find(|r| r.id == request_id) {
            if let Some(prev) = rec.detail.take() {
                if detail.request_headers.is_empty() {
                    detail.request_headers = prev.request_headers;
                }
                if detail.response_headers.is_empty() {
                    detail.response_headers = prev.response_headers;
                }
                if detail.request_body.is_none() {
                    detail.request_body = prev.request_body;
                }
                if detail.response_body.is_none() {
                    detail.response_body = prev.response_body;
                }
                if detail.remote_address.is_none() {
                    detail.remote_address = prev.remote_address;
                }
                if detail.local_port.is_none() {
                    detail.local_port = prev.local_port;
                }
            }
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
            let has_body = detail.request_body.is_some() || detail.response_body.is_some();
            rec.detail = Some(detail);
            if rec.response_complete || has_body {
                rec.body_loaded = true;
            }
        }
        if self.selected_id.as_deref() == Some(request_id) {
            self.rebuild_selected_body_cache();
        }
    }

    pub fn select_request(&mut self, request_id: String) -> Option<(String, Option<String>)> {
        self.selected_id = Some(request_id.clone());
        self.rebuild_selected_body_cache();
        let rec = self.requests.iter().find(|r| r.id == request_id)?;
        let has_error = self.detail_errors.contains_key(&request_id);
        if !rec.body_loaded || has_error {
            let id = rec.id.clone();
            let iso = rec.isolate_id.clone();
            self.mark_detail_loading(&id);
            Some((id, iso))
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.requests.clear();
        self.filtered_indices.clear();
        self.selected_id = None;
        self.detail_loading_id = None;
        self.detail_errors.clear();
        self.cached_detail_id = None;
        self.cached_req_body = None;
        self.cached_resp_body = None;
    }

    pub fn selected_record(&self) -> Option<&HttpRequestRecord> {
        let id = self.selected_id.as_deref()?;
        self.requests.iter().find(|r| r.id == id)
    }

    fn rebuild_selected_body_cache(&mut self) {
        let Some(rec) = self.selected_id.as_deref().and_then(|id| self.requests.iter().find(|r| r.id == id)) else {
            self.cached_detail_id = None;
            self.cached_req_body = None;
            self.cached_resp_body = None;
            return;
        };
        self.cached_detail_id = Some(rec.id.clone());
        let detail = rec.detail.as_ref();
        self.cached_req_body = detail.and_then(|d| d.request_body.as_deref()).filter(|s| !s.trim().is_empty()).map(format_and_tokenize_json);
        self.cached_resp_body = detail.and_then(|d| d.response_body.as_deref()).filter(|s| !s.trim().is_empty()).map(format_and_tokenize_json);
    }

    pub fn rebuild_filtered_indices(&mut self) {
        let q = self.search_query.trim().to_ascii_lowercase();
        let chip = self.filter_chip;
        let mut indices: Vec<usize> = self
            .requests
            .iter()
            .enumerate()
            .filter(|(_, rec)| chip.matches(rec))
            .filter(|(_, rec)| {
                q.is_empty()
                    || rec.uri.to_ascii_lowercase().contains(&q)
                    || rec.method.to_ascii_lowercase().contains(&q)
                    || rec.status_code.is_some_and(|s| s.to_string().contains(&q))
                    || rec.mime_short.to_ascii_lowercase().contains(&q)
            })
            .map(|(idx, _)| idx)
            .collect();

        let (sort_col, sort_asc, reqs) = (self.sort_column, self.sort_ascending, &self.requests);
        indices.sort_by(|&ia, &ib| {
            let (a, b) = (&reqs[ia], &reqs[ib]);
            let ord = match sort_col {
                NetworkSortColumn::Timestamp => a.start_time_micros.cmp(&b.start_time_micros),
                NetworkSortColumn::Method => a.method.cmp(&b.method),
                NetworkSortColumn::Address => a.path_and_query.cmp(&b.path_and_query),
                NetworkSortColumn::Status => a.status_code.unwrap_or(0).cmp(&b.status_code.unwrap_or(0)),
                NetworkSortColumn::Type => a.mime_short.cmp(&b.mime_short),
                NetworkSortColumn::Duration => a.duration_ms.unwrap_or(0).cmp(&b.duration_ms.unwrap_or(0)),
                NetworkSortColumn::Size => a.total_bytes().cmp(&b.total_bytes()),
            };
            if sort_asc { ord } else { ord.reverse() }
        });
        self.filtered_indices = indices;
    }
}

impl RootView {
    pub fn render_network_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let recording = self.network.recording;
        let current_chip = self.network.filter_chip;
        let filtered_count = self.network.filtered_indices.len();
        let total_count = self.network.requests.len();

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
                                Button::new("net-record-btn")
                                    .small()
                                    .compact()
                                    .selected(recording)
                                    .icon(Icon::new(if recording {
                                        IconName::CircleDot
                                    } else {
                                        IconName::Pause
                                    }))
                                    .label(if recording {
                                        "Recording"
                                    } else {
                                        "Paused"
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
                                    .icon(Icon::new(IconName::Trash))
                                    .label("Clear")
                                    .tooltip("Clear captured HTTP requests")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.network.clear();
                                        this.send_cmd(BackendCommand::ClearNetworkProfile);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div().w(px(240.0)).child(
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
                                    this.network.set_filter_chip(chip);
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
                                    .child(format!("{filtered_count} / {total_count} requests")),
                            )
                            .child(
                                Button::new("net-copy-all-btn")
                                    .small()
                                    .compact()
                                    .icon(Icon::new(IconName::ClipboardCopy))
                                    .label("Copy All")
                                    .tooltip("Copy complete request & response report (General, Headers, and Bodies)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(rec) = this.network.selected_record().cloned() {
                                            if !rec.body_loaded {
                                                this.network.mark_detail_loading(&rec.id);
                                                this.send_cmd(
                                                    BackendCommand::FetchHttpRequestDetail {
                                                        request_id: rec.id.clone(),
                                                        isolate_id: rec.isolate_id.clone(),
                                                    },
                                                );
                                            }
                                            let report = rec.to_full_text_report();
                                            this.copy_to_clipboard(
                                                report,
                                                "Copied complete HTTP request & response info",
                                                window,
                                                cx,
                                            );
                                        } else {
                                            this.show_toast(
                                                "Select a request first to copy all details",
                                                true,
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                            )
                            .child(
                                Button::new("net-copy-curl-btn")
                                    .small()
                                    .compact()
                                    .icon(Icon::new(IconName::Copy))
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
            .child(
                div().flex_1().min_h_0().w_full().child(
                    h_resizable("network-split-pane")
                        .child(
                            resizable_panel()
                                .size(px(680.0))
                                .size_range(px(380.0)..px(1100.0))
                                .child(render_requests_table(self, cx)),
                        )
                        .child(resizable_panel().child(render_request_detail_pane(self, cx))),
                ),
            )
    }
}

pub(crate) fn merge_requests_into_deque(
    requests: &mut VecDeque<HttpRequestRecord>,
    batch: Vec<HttpRequestRecord>,
    selected_id: Option<&str>,
) -> Option<(String, Option<String>)> {
    let mut refetch_selected = None;
    for incoming in batch {
        if let Some(existing) = requests.iter_mut().find(|r| r.id == incoming.id) {
            let prev_complete = existing.response_complete;
            let prev_detail = existing.detail.take();
            let prev_body_loaded = existing.body_loaded;
            let prev_iso = existing.isolate_id.take();
            *existing = incoming;
            if existing.isolate_id.is_none() {
                existing.isolate_id = prev_iso;
            }
            if !prev_complete && existing.response_complete {
                existing.body_loaded = false;
                if existing.detail.is_none() {
                    existing.detail = prev_detail;
                }
            } else if !existing.body_loaded && prev_body_loaded {
                existing.detail = prev_detail;
                existing.body_loaded = true;
            } else if existing.detail.is_none() {
                existing.detail = prev_detail;
            }
            if selected_id == Some(existing.id.as_str())
                && !existing.body_loaded
                && (existing.response_complete || !existing.is_pending())
            {
                refetch_selected = Some((existing.id.clone(), existing.isolate_id.clone()));
            }
        } else {
            if requests.len() >= MAX_NETWORK_REQUESTS {
                requests.pop_front();
            }
            requests.push_back(incoming);
        }
    }
    refetch_selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn streaming_request_resets_body_loaded_and_refetches_when_response_completes() {
        let partial = HttpRequestRecord::parse_from_profile_entry(&json!({
            "id": "req-stream-1", "method": "GET", "uri": "https://api.example.com/stream",
            "startTime": 1_000_000, "endTime": 1_010_000,
            "responseData": { "statusCode": 200 }
        }))
        .unwrap();
        assert!(!partial.response_complete);

        let mut deque = VecDeque::new();
        assert_eq!(merge_requests_into_deque(&mut deque, vec![partial], Some("req-stream-1")), None);
        // Simulate premature detail load while streaming response was still incomplete
        deque[0].body_loaded = true;
        deque[0].detail = Some(HttpRequestDetail::default());

        let completed = HttpRequestRecord::parse_from_profile_entry(&json!({
            "id": "req-stream-1", "method": "GET", "uri": "https://api.example.com/stream",
            "startTime": 1_000_000, "endTime": 1_010_000,
            "responseData": { "statusCode": 200, "endTime": 1_500_000 }
        }))
        .unwrap();
        assert!(completed.response_complete);

        let refetch = merge_requests_into_deque(&mut deque, vec![completed], Some("req-stream-1"));
        assert_eq!(
            refetch.as_ref().map(|(id, _)| id.as_str()),
            Some("req-stream-1")
        );
        assert!(!deque[0].body_loaded);
    }
}
