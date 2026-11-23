use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _,
    component::{
        Sizable as _,
        button::Button,
        scroll::ScrollableElement as _,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::protocol::{
    FormattedBody, HttpRequestDetail, HttpRequestRecord, JsonTokenKind,
};
use crate::theme::{DevToolsTheme, format_bytes, format_duration_ms};
use crate::ui::app::RootView;
use crate::ui::components::{empty_placeholder, kv_row};
use crate::ui::tabs::network::NetworkDetailSubTab;

pub fn render_request_detail_pane(
    view: &RootView,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let Some(rec) = view.network.selected_record().cloned() else {
        return empty_placeholder("No request selected").into_any_element();
    };

    let active_sub_tab = view.network.detail_tab;
    let detail = rec.detail.clone().unwrap_or_default();
    let cached_req = view.network.cached_req_body.clone();
    let cached_resp = view.network.cached_resp_body.clone();

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
        .child(
            div()
                .id("net-detail-body-scroll")
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
                    NetworkDetailSubTab::Request => {
                        render_cached_body_subtab("Request Body", cached_req, cx).into_any_element()
                    }
                    NetworkDetailSubTab::Response => {
                        render_cached_body_subtab("Response Body", cached_resp, cx)
                            .into_any_element()
                    }
                }),
        )
        .into_any_element()
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
    cached: Option<FormattedBody>,
    cx: &mut Context<RootView>,
) -> impl IntoElement + use<> {
    let Some(body) = cached else {
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

    let copy_payload = body.full_copy_text;
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
                            body.total_lines,
                            format_bytes(body.raw_len as u64)
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
                .flex()
                .flex_col()
                .gap_0p5()
                .children(body.lines.into_iter().enumerate().map(|(idx, line)| {
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
