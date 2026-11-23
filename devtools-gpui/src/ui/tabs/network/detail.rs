use gpui_kit::{
    Context, HighlightStyle, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _,
    assets::IconName,
    base::SelectableText,
    component::{
        Icon, Sizable as _,
        button::Button,
        scroll::ScrollableElement as _,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::backend::BackendCommand;
use crate::protocol::{
    FormattedBody, HttpRequestDetail, HttpRequestRecord, JsonTokenKind,
    WebSocketConnectionDetail,
};
use crate::theme::{
    DevToolsTheme, format_bytes, format_duration_ms, format_micros_local_hms,
};
use crate::ui::app::RootView;
use crate::ui::components::{empty_placeholder, kv_row, pill_badge};
use crate::ui::tabs::network::NetworkDetailSubTab;

pub fn render_request_detail_pane(
    view: &RootView,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let Some(rec) = view.network.selected_record().cloned() else {
        return empty_placeholder("Select an HTTP request or WebSocket connection to inspect")
            .into_any_element();
    };

    if rec.is_websocket() {
        return render_websocket_detail_pane(view, &rec, cx).into_any_element();
    }

    let active_sub_tab = view.network.detail_tab;
    let detail = rec.detail.clone().unwrap_or_default();
    let cached_req = view.network.cached_req_body.clone();
    let cached_resp = view.network.cached_resp_body.clone();
    let is_loading = view.network.detail_loading_id.as_deref() == Some(rec.id.as_str());
    let detail_error = view.network.detail_errors.get(&rec.id).cloned();
    let rec_for_copy = rec.clone();

    div()
        .size_full()
        .bg(DevToolsTheme::bg_panel())
        .flex()
        .flex_col()
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
                            let req_id_for_tab = rec.id.clone();
                            let iso_for_tab = rec.isolate_id.clone();
                            let body_loaded_now = rec.body_loaded;
                            let has_err_now = detail_error.is_some();
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
                                    if matches!(
                                        tab,
                                        NetworkDetailSubTab::Request
                                            | NetworkDetailSubTab::Response
                                            | NetworkDetailSubTab::Headers
                                    ) && (!body_loaded_now || has_err_now)
                                    {
                                        this.network.mark_detail_loading(&req_id_for_tab);
                                        this.send_cmd(BackendCommand::FetchHttpRequestDetail {
                                            request_id: req_id_for_tab.clone(),
                                            isolate_id: iso_for_tab.clone(),
                                        });
                                    }
                                    cx.notify();
                                }))
                                .child(tab.label())
                        })),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("net-detail-copy-all-btn")
                                .small()
                                .compact()
                                .icon(Icon::new(IconName::ClipboardCopy))
                                .label("Copy All")
                                .tooltip("Copy complete request & response report to clipboard")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let latest_rec = this
                                        .network
                                        .selected_record()
                                        .cloned()
                                        .unwrap_or_else(|| rec_for_copy.clone());
                                    if !latest_rec.body_loaded {
                                        this.network.mark_detail_loading(&latest_rec.id);
                                        this.send_cmd(BackendCommand::FetchHttpRequestDetail {
                                            request_id: latest_rec.id.clone(),
                                            isolate_id: latest_rec.isolate_id.clone(),
                                        });
                                    }
                                    this.copy_to_clipboard(
                                        latest_rec.to_full_text_report(),
                                        "Copied complete HTTP request & response info",
                                        window,
                                        cx,
                                    );
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
                ),
        )
        .child(
            div()
                .id("net-detail-body-scroll")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_3()
                .overflow_y_scrollbar()
                .child(match active_sub_tab {
                    NetworkDetailSubTab::Overview => {
                        render_overview_subtab(&rec, &detail).into_any_element()
                    }
                    NetworkDetailSubTab::Headers => {
                        render_headers_subtab(&detail).into_any_element()
                    }
                    NetworkDetailSubTab::Request => render_cached_body_subtab(
                        "Request Body",
                        &rec,
                        cached_req,
                        is_loading,
                        detail_error,
                        cx,
                    )
                    .into_any_element(),
                    NetworkDetailSubTab::Response => render_cached_body_subtab(
                        "Response Body",
                        &rec,
                        cached_resp,
                        is_loading,
                        detail_error,
                        cx,
                    )
                    .into_any_element(),
                }),
        )
        .into_any_element()
}

fn render_websocket_detail_pane(
    view: &RootView,
    rec: &HttpRequestRecord,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let ws = rec.ws_detail.clone().unwrap_or_default();
    let is_overview = view.network.detail_tab == NetworkDetailSubTab::Overview;
    let (status_badge, status_color) = rec.status_badge_label_and_color();
    let rec_for_copy = rec.clone();
    let req_id_refresh = rec.id.clone();
    let iso_refresh = rec.isolate_id.clone();
    let events_count = ws.events.len();

    div()
        .size_full()
        .bg(DevToolsTheme::bg_panel())
        .flex()
        .flex_col()
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
                        .child(
                            div()
                                .id("ws-detail-tab-overview")
                                .h_full()
                                .px_2p5()
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .border_b_2()
                                .text_xs()
                                .when(is_overview, |el| {
                                    el.border_color(DevToolsTheme::accent_blue())
                                        .text_color(DevToolsTheme::text_primary())
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                })
                                .when(!is_overview, |el| {
                                    el.border_color(gpui_kit::transparent_black())
                                        .text_color(DevToolsTheme::text_secondary())
                                        .hover(|h| h.text_color(DevToolsTheme::text_primary()))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.network.detail_tab = NetworkDetailSubTab::Overview;
                                    cx.notify();
                                }))
                                .child("Overview"),
                        )
                        .child(
                            div()
                                .id("ws-detail-tab-messages")
                                .h_full()
                                .px_2p5()
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .border_b_2()
                                .text_xs()
                                .when(!is_overview, |el| {
                                    el.border_color(DevToolsTheme::accent_blue())
                                        .text_color(DevToolsTheme::text_primary())
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                })
                                .when(is_overview, |el| {
                                    el.border_color(gpui_kit::transparent_black())
                                        .text_color(DevToolsTheme::text_secondary())
                                        .hover(|h| h.text_color(DevToolsTheme::text_primary()))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.network.detail_tab = NetworkDetailSubTab::Response;
                                    cx.notify();
                                }))
                                .child(format!("Messages & Events ({events_count})")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("ws-detail-refresh-btn")
                                .small()
                                .compact()
                                .icon(Icon::new(IconName::RefreshCw))
                                .label("Refresh")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.network.mark_detail_loading(&req_id_refresh);
                                    this.send_cmd(BackendCommand::FetchHttpRequestDetail {
                                        request_id: req_id_refresh.clone(),
                                        isolate_id: iso_refresh.clone(),
                                    });
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("ws-detail-copy-all-btn")
                                .small()
                                .compact()
                                .icon(Icon::new(IconName::ClipboardCopy))
                                .label("Copy All")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let latest = this
                                        .network
                                        .selected_record()
                                        .cloned()
                                        .unwrap_or_else(|| rec_for_copy.clone());
                                    this.copy_to_clipboard(
                                        latest.to_full_text_report(),
                                        "Copied WebSocket connection & events report",
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(pill_badge(&format!("WS {status_badge}"), status_color, 0.2)),
                ),
        )
        .child(
            div()
                .id("ws-detail-body-scroll")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_3()
                .overflow_y_scrollbar()
                .child(if is_overview {
                    render_websocket_overview_subtab(rec, &ws).into_any_element()
                } else {
                    render_websocket_messages_subtab(rec, &ws).into_any_element()
                }),
        )
}

fn render_websocket_overview_subtab(
    rec: &HttpRequestRecord,
    ws: &WebSocketConnectionDetail,
) -> impl IntoElement + use<> {
    let open_ts_str = ws
        .open_timestamp
        .map(format_micros_local_hms)
        .unwrap_or_else(|| "-".to_string());
    let close_ts_str = ws
        .close_timestamp
        .map(format_micros_local_hms)
        .unwrap_or_else(|| "-".to_string());

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
                        .text_color(DevToolsTheme::accent_cyan())
                        .child("WebSocket Connection"),
                )
                .child(kv_row("WebSocket URL", &rec.uri))
                .child(kv_row("State", &ws.state))
                .child(kv_row("Protocol", ws.protocol.as_deref().unwrap_or("-")))
                .child(kv_row("Duration", &rec.duration_display()))
                .child(kv_row("Connect Timestamp", &rec.timestamp_hms))
                .child(kv_row("Open Timestamp", &open_ts_str))
                .child(kv_row("Close Timestamp", &close_ts_str))
                .when_some(ws.close_code, |el, code| {
                    el.child(kv_row("Close Code", &code.to_string()))
                })
                .when_some(ws.close_reason.as_deref(), |el, reason| {
                    el.child(kv_row("Close Reason", reason))
                })
                .when_some(
                    ws.error.as_deref().or(rec.error.as_deref()),
                    |el, err| el.child(kv_row("Error", err)),
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
                        .child("Frames & Traffic Metrics"),
                )
                .child(kv_row("Frames Sent", &ws.frames_sent.to_string()))
                .child(kv_row("Frames Received", &ws.frames_received.to_string()))
                .child(kv_row("Bytes Sent", &format_bytes(ws.bytes_sent)))
                .child(kv_row("Bytes Received", &format_bytes(ws.bytes_received)))
                .child(kv_row("Total Traffic", &format_bytes(rec.total_bytes())))
                .child(kv_row("Ping Count", &ws.ping_count.to_string()))
                .child(kv_row("Pong Count", &ws.pong_count.to_string())),
        )
}

fn render_websocket_messages_subtab(
    rec: &HttpRequestRecord,
    ws: &WebSocketConnectionDetail,
) -> impl IntoElement + use<> {
    let connect_ts = ws.connect_timestamp;
    let events = ws.events.clone();
    let rec_id = rec.id.clone();

    div()
        .w_full()
        .p_3()
        .rounded(px(6.0))
        .bg(DevToolsTheme::bg_main())
        .border_1()
        .border_color(DevToolsTheme::border())
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
                        .text_color(DevToolsTheme::accent_cyan())
                        .child(format!(
                            "WebSocket Timeline Events & Frames ({})",
                            events.len()
                        )),
                )
                .child(
                    div()
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .text_color(DevToolsTheme::text_secondary())
                        .child(format!(
                            "Sent: {} ({} frames) · Received: {} ({} frames)",
                            format_bytes(ws.bytes_sent),
                            ws.frames_sent,
                            format_bytes(ws.bytes_received),
                            ws.frames_received
                        )),
                ),
        )
        .when(events.is_empty(), |el| {
            el.child(
                div()
                    .py_6()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(DevToolsTheme::text_muted())
                    .child("No WebSocket events or frames recorded yet."),
            )
        })
        .children(events.into_iter().enumerate().map(|(idx, ev)| {
            let is_out = ev.direction.as_deref() == Some("out");
            let is_in = ev.direction.as_deref() == Some("in");
            let is_err = ev.event_name.contains("Error") || ev.error_message.is_some();
            let dir_color = if is_err {
                DevToolsTheme::error_red()
            } else if is_out {
                DevToolsTheme::accent_cyan()
            } else if is_in {
                DevToolsTheme::success_green()
            } else {
                DevToolsTheme::accent_purple()
            };
            let dir_icon = if is_out {
                IconName::ArrowUp
            } else if is_in {
                IconName::ArrowDown
            } else {
                IconName::CircleDot
            };
            let rel_ms = ev.timestamp_micros.saturating_sub(connect_ts) / 1000;

            div()
                .w_full()
                .p_2()
                .rounded(px(4.0))
                .bg(DevToolsTheme::bg_panel())
                .border_1()
                .border_color(DevToolsTheme::border())
                .font_family(DevToolsTheme::MONO_FONT)
                .text_xs()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Icon::new(dir_icon).xsmall().text_color(dir_color))
                                .child(
                                    div()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(dir_color)
                                        .child(SelectableText::new(
                                            SharedString::from(format!(
                                                "ws-ev-name-{rec_id}-{idx}"
                                            )),
                                            ev.event_name.clone(),
                                        )),
                                )
                                .when_some(ev.frame_number, |el, fnum| {
                                    el.child(pill_badge(
                                        &format!("Frame #{fnum}"),
                                        DevToolsTheme::accent_blue(),
                                        0.18,
                                    ))
                                })
                                .when_some(ev.opcode.as_deref(), |el, op| {
                                    el.child(pill_badge(op, dir_color, 0.18))
                                })
                                .when_some(ev.payload_size, |el, sz| {
                                    el.child(
                                        div()
                                            .text_color(DevToolsTheme::text_primary())
                                            .child(SelectableText::new(
                                                SharedString::from(format!(
                                                    "ws-ev-sz-{rec_id}-{idx}"
                                                )),
                                                format_bytes(sz),
                                            )),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_color(DevToolsTheme::text_muted())
                                .child(SelectableText::new(
                                    SharedString::from(format!("ws-ev-ts-{rec_id}-{idx}")),
                                    format!("{} (+{rel_ms} ms)", ev.timestamp_hms),
                                )),
                        ),
                )
                .when_some(ev.error_message.clone(), |el, err_msg| {
                    let full_err = match &ev.error_type {
                        Some(et) => format!("{et}: {err_msg}"),
                        None => err_msg,
                    };
                    el.child(
                        div()
                            .text_color(DevToolsTheme::error_red())
                            .child(SelectableText::new(
                                SharedString::from(format!("ws-ev-err-{rec_id}-{idx}")),
                                full_err,
                            )),
                    )
                })
                .when_some(ev.arguments_json.clone(), |el, args_str| {
                    el.child(
                        div()
                            .text_color(DevToolsTheme::text_secondary())
                            .child(SelectableText::new(
                                SharedString::from(format!("ws-ev-args-{rec_id}-{idx}")),
                                args_str,
                            )),
                    )
                })
        }))
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

fn render_cached_body_subtab(
    title: &str,
    rec: &HttpRequestRecord,
    cached: Option<FormattedBody>,
    is_loading: bool,
    detail_error: Option<String>,
    cx: &mut Context<RootView>,
) -> impl IntoElement + use<> {
    let Some(body) = cached else {
        let req_id = rec.id.clone();
        let iso_id = rec.isolate_id.clone();
        if is_loading {
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
                .text_color(DevToolsTheme::accent_cyan())
                .child(format!("Loading {title} from Dart VM Service..."));
        }

        if let Some(err_msg) = detail_error {
            return div()
                .w_full()
                .p_6()
                .rounded(px(6.0))
                .bg(DevToolsTheme::bg_main())
                .border_1()
                .border_color(DevToolsTheme::border())
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2p5()
                .text_xs()
                .child(
                    div()
                        .text_color(DevToolsTheme::error_red())
                        .child(format!("Failed to fetch {title}: {err_msg}")),
                )
                .child(
                    Button::new(SharedString::from(format!("btn-retry-{title}-{}", rec.id)))
                        .small()
                        .compact()
                        .icon(Icon::new(IconName::RefreshCw))
                        .label("Retry")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.network.mark_detail_loading(&req_id);
                            this.send_cmd(BackendCommand::FetchHttpRequestDetail {
                                request_id: req_id.clone(),
                                isolate_id: iso_id.clone(),
                            });
                            cx.notify();
                        })),
                );
        }

        let empty_msg = if !rec.body_loaded {
            format!("{title} not fetched yet — click Retry to load from VM Service.")
        } else {
            format!("Empty {title} (0 bytes)")
        };

        return div()
            .w_full()
            .p_6()
            .rounded(px(6.0))
            .bg(DevToolsTheme::bg_main())
            .border_1()
            .border_color(DevToolsTheme::border())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .text_xs()
            .text_color(DevToolsTheme::text_muted())
            .child(empty_msg)
            .when(!rec.body_loaded, |el| {
                let req_id_load = rec.id.clone();
                let iso_id_load = rec.isolate_id.clone();
                el.child(
                    Button::new(SharedString::from(format!("btn-fetch-{title}-{}", rec.id)))
                        .small()
                        .compact()
                        .icon(Icon::new(IconName::RefreshCw))
                        .label("Fetch Body")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.network.mark_detail_loading(&req_id_load);
                            this.send_cmd(BackendCommand::FetchHttpRequestDetail {
                                request_id: req_id_load.clone(),
                                isolate_id: iso_id_load.clone(),
                            });
                            cx.notify();
                        })),
                )
            });
    };

    let (selectable_str, token_ranges) = body.to_selectable_text_and_highlights();
    let gpui_highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = token_ranges
        .into_iter()
        .map(|(range, kind)| {
            let color = match kind {
                JsonTokenKind::Punctuation => DevToolsTheme::text_secondary(),
                JsonTokenKind::Key => DevToolsTheme::accent_blue(),
                JsonTokenKind::StringVal => DevToolsTheme::success_green(),
                JsonTokenKind::NumberVal => DevToolsTheme::warning_yellow(),
                JsonTokenKind::BoolOrNull => DevToolsTheme::accent_purple(),
            };
            (
                range,
                HighlightStyle {
                    color: Some(color.into()),
                    ..Default::default()
                },
            )
        })
        .collect();

    let copy_payload = body.full_copy_text;
    let title_owned = title.to_string();
    let selectable_id = SharedString::from(format!(
        "net-body-sel-{}-{}-{}",
        rec.id, title, body.raw_len
    ));

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
                            body.total_lines,
                            format_bytes(body.raw_len as u64)
                        )),
                )
                .child(
                    Button::new(SharedString::from(format!("btn-copy-{title}")))
                        .small()
                        .compact()
                        .icon(Icon::new(IconName::Copy))
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
        .when(body.truncated, |el| {
            el.child(
                div()
                    .px_3()
                    .py_1p5()
                    .rounded(px(4.0))
                    .bg(DevToolsTheme::to_hsla(DevToolsTheme::warning_yellow(), 0.16))
                    .text_xs()
                    .text_color(DevToolsTheme::warning_yellow())
                    .child("Large payload preview truncated for smooth 120fps rendering. Click 'Copy Body' to copy the full content."),
            )
        })
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
                .text_color(DevToolsTheme::text_primary())
                .child(
                    SelectableText::new(selectable_id, selectable_str).highlights(gpui_highlights),
                ),
        )
}
