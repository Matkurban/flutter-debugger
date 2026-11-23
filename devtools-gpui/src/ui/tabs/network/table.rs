use std::ops::Range;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window,
    component::scroll::ScrollableElement as _,
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};

use crate::backend::BackendCommand;
use crate::protocol::HttpRequestRecord;
use crate::theme::{DevToolsTheme, format_bytes, format_duration_ms};
use crate::ui::app::RootView;
use crate::ui::tabs::network::NetworkSortColumn;

pub fn render_requests_table(
    view: &RootView,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let sort_col = view.network.sort_column;
    let sort_asc = view.network.sort_ascending;
    let row_count = view.network.filtered_indices.len();
    let scroll_handle = view.network.scroll_handle.clone();

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
        .child(if row_count == 0 {
            div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(DevToolsTheme::text_muted())
                .child("No HTTP requests matching current filter.")
                .into_any_element()
        } else {
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    uniform_list(
                        "network-uniform-list",
                        row_count,
                        cx.processor(
                            move |this: &mut RootView,
                                  visible_range: Range<usize>,
                                  _window: &mut Window,
                                  cx: &mut Context<RootView>| {
                                let selected_id = this.network.selected_id.clone();
                                let mut items = Vec::with_capacity(visible_range.len());
                                for row_ix in visible_range {
                                    let Some(&req_idx) = this.network.filtered_indices.get(row_ix)
                                    else {
                                        continue;
                                    };
                                    let Some(rec) = this.network.requests.get(req_idx) else {
                                        continue;
                                    };
                                    let is_selected =
                                        selected_id.as_deref() == Some(rec.id.as_str());
                                    items.push(render_single_request_row(rec, is_selected, cx));
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
        })
}

fn render_single_request_row(
    rec: &HttpRequestRecord,
    is_selected: bool,
    cx: &mut Context<RootView>,
) -> impl IntoElement + use<> {
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
            let needs_fetch = this.network.select_request(req_id.clone());
            if needs_fetch {
                this.send_cmd(BackendCommand::FetchHttpRequestDetail {
                    request_id: req_id.clone(),
                });
            }
            cx.notify();
        }))
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
        .child(
            div()
                .w(px(68.0))
                .text_color(status_color)
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(status_label),
        )
        .child(
            div()
                .w(px(56.0))
                .text_color(DevToolsTheme::text_secondary())
                .child(rec.mime_short.clone()),
        )
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
        .child(
            div()
                .w(px(76.0))
                .text_color(DevToolsTheme::text_secondary())
                .child(format_bytes(rec.total_bytes())),
        )
        .child(
            div()
                .w(px(92.0))
                .text_color(DevToolsTheme::text_muted())
                .child(rec.timestamp_hms.clone()),
        )
}
