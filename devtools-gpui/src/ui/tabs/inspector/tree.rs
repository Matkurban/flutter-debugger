use std::ops::Range;

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, MouseDownEvent, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::{Icon, Sizable as _, scroll::ScrollableElement as _},
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};

use crate::backend::BackendCommand;
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;
use crate::ui::tabs::inspector::FlatWidgetRow;

pub fn render_widget_tree_list(
    view: &RootView,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let row_count = view.inspector.flat_rows.len();
    let scroll_handle = view.inspector.scroll_handle.clone();
    let breadcrumbs = view.inspector.selected_ancestor_breadcrumbs();

    if row_count == 0 {
        return div()
            .size_full()
            .bg(DevToolsTheme::bg_main())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .text_xs()
            .text_color(DevToolsTheme::text_muted())
            .child("No Widget tree loaded yet.")
            .child("Click 'Refresh Tree' once your Flutter application is running.")
            .into_any_element();
    }

    div()
        .size_full()
        .bg(DevToolsTheme::bg_main())
        .flex()
        .flex_col()
        .when(!breadcrumbs.is_empty(), |el| {
            el.child(render_breadcrumb_bar(breadcrumbs, cx))
        })
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    uniform_list(
                        "inspector-uniform-list",
                        row_count,
                        cx.processor(
                            move |this: &mut RootView,
                                  visible_range: Range<usize>,
                                  _window: &mut Window,
                                  cx: &mut Context<RootView>| {
                                let selected_id = this.inspector.selected_value_id.clone();
                                let mut items = Vec::with_capacity(visible_range.len());
                                for row_ix in visible_range {
                                    let Some(row) = this.inspector.flat_rows.get(row_ix) else {
                                        continue;
                                    };
                                    let is_selected = !row.value_id.is_empty()
                                        && selected_id.as_deref() == Some(row.value_id.as_str());
                                    items.push(render_single_widget_row(
                                        row_ix,
                                        row,
                                        is_selected,
                                        cx,
                                    ));
                                }
                                items
                            },
                        ),
                    )
                    .size_full()
                    .track_scroll(&scroll_handle),
                )
                .vertical_scrollbar(&scroll_handle),
        )
        .into_any_element()
}

fn render_breadcrumb_bar(
    breadcrumbs: Vec<(String, String)>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let last_idx = breadcrumbs.len().saturating_sub(1);
    div()
        .id("inspector-breadcrumb-bar")
        .w_full()
        .h(px(28.0))
        .px_2p5()
        .bg(DevToolsTheme::bg_toolbar())
        .border_b_1()
        .border_color(DevToolsTheme::border())
        .font_family(DevToolsTheme::MONO_FONT)
        .text_xs()
        .flex()
        .items_center()
        .gap_1()
        .overflow_x_hidden()
        .children(
            breadcrumbs
                .into_iter()
                .enumerate()
                .map(|(idx, (vid, label))| {
                    let is_last = idx == last_idx;
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .flex_shrink_0()
                        .child(
                            div()
                                .id(SharedString::from(format!("bc-item-{idx}-{vid}")))
                                .px_1p5()
                                .py(px(1.0))
                                .rounded(px(3.0))
                                .cursor_pointer()
                                .when(is_last, |el| {
                                    el.bg(DevToolsTheme::bg_selected())
                                        .text_color(DevToolsTheme::accent_blue())
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                })
                                .when(!is_last, |el| {
                                    el.text_color(DevToolsTheme::text_secondary())
                                        .hover(|h| {
                                            h.bg(DevToolsTheme::bg_hover())
                                                .text_color(DevToolsTheme::text_primary())
                                        })
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let _ = this.inspector.select_node(&vid);
                                    if let Some(row_ix) = this
                                        .inspector
                                        .flat_rows
                                        .iter()
                                        .position(|r| r.value_id == vid)
                                    {
                                        this.inspector.scroll_handle.scroll_to_item(
                                            row_ix,
                                            gpui_kit::ScrollStrategy::Center,
                                        );
                                    }
                                    if !vid.is_empty() {
                                        this.send_cmd(BackendCommand::SelectWidgetNode {
                                            value_id: vid.clone(),
                                            highlight_on_device: this.overlays.inspector,
                                        });
                                    }
                                    cx.notify();
                                }))
                                .child(label),
                        )
                        .when(!is_last, |el| {
                            el.child(
                                Icon::new(IconName::ChevronRight)
                                    .xsmall()
                                    .text_color(DevToolsTheme::text_muted()),
                            )
                        })
                }),
        )
}

fn render_single_widget_row(
    idx: usize,
    row: &FlatWidgetRow,
    is_selected: bool,
    cx: &mut Context<RootView>,
) -> impl IntoElement + use<> {
    let vid = row.value_id.clone();
    let vid_double = row.value_id.clone();
    let vid_toggle = row.value_id.clone();
    let indent_px = (row.depth as f32) * 14.0 + 6.0;
    let has_children = row.has_children;
    let is_expanded = row.expanded;

    div()
        .id(SharedString::from(format!(
            "widget-row-{idx}-{}",
            row.value_id
        )))
        .w_full()
        .h(px(28.0))
        .pl(px(indent_px))
        .pr_3()
        .border_b_1()
        .border_color(DevToolsTheme::border())
        .cursor_pointer()
        .font_family(DevToolsTheme::MONO_FONT)
        .text_xs()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .when(is_selected, |el| el.bg(DevToolsTheme::bg_selected()))
        .when(!is_selected, |el| {
            el.hover(|h| h.bg(DevToolsTheme::bg_hover()))
        })
        // Single-click ONLY selects the node and loads properties/layout/details (NEVER jumps to Zed).
        .on_click(cx.listener(move |this, _, _, cx| {
            let _ = this.inspector.select_node(&vid);
            if !vid.is_empty() {
                this.send_cmd(BackendCommand::SelectWidgetNode {
                    value_id: vid.clone(),
                    highlight_on_device: this.overlays.inspector,
                });
            }
            cx.notify();
        }))
        // Explicit double-click jumps to source in Zed.
        .on_mouse_down(
            gpui_kit::MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                if ev.click_count >= 2
                    && let Some(node) = this.inspector.find_node(&vid_double)
                    && let Some(loc) = this.inspector.resolve_jump_location(node)
                {
                    this.jump_to_zed(loc);
                }
            }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .id(SharedString::from(format!("widget-arrow-{idx}")))
                        .w(px(16.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(DevToolsTheme::text_secondary())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            let lazy_load = this.inspector.toggle_expand(&vid_toggle);
                            if lazy_load && !vid_toggle.is_empty() {
                                this.send_cmd(BackendCommand::ExpandWidgetChildren {
                                    value_id: vid_toggle.clone(),
                                });
                            }
                            cx.notify();
                        }))
                        .when(has_children, |el| {
                            el.child(
                                Icon::new(if is_expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .xsmall(),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(if row.created_by_local_project {
                            gpui_kit::FontWeight::BOLD
                        } else {
                            gpui_kit::FontWeight::NORMAL
                        })
                        .text_color(if row.created_by_local_project {
                            DevToolsTheme::accent_blue()
                        } else {
                            DevToolsTheme::text_primary()
                        })
                        .child(row.widget_runtime_type.clone()),
                )
                .when(
                    row.description != row.widget_runtime_type && !row.description.is_empty(),
                    |el| {
                        el.child(
                            div()
                                .text_color(DevToolsTheme::text_secondary())
                                .truncate()
                                .child(row.description.clone()),
                        )
                    },
                ),
        )
        .when_some(row.location.clone(), |el, loc| {
            let loc_jump = loc.clone();
            el.child(
                div()
                    .id(SharedString::from(format!("widget-loc-{idx}")))
                    .flex_shrink_0()
                    .px_1p5()
                    .py(px(1.0))
                    .rounded(px(3.0))
                    .bg(if row.created_by_local_project {
                        DevToolsTheme::to_hsla(DevToolsTheme::accent_blue(), 0.18)
                    } else {
                        DevToolsTheme::bg_elevated().into()
                    })
                    .text_color(if row.created_by_local_project {
                        DevToolsTheme::accent_cyan()
                    } else {
                        DevToolsTheme::text_secondary()
                    })
                    .hover(|h| h.bg(DevToolsTheme::bg_selected()))
                    .flex()
                    .items_center()
                    .gap_1()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.jump_to_zed(loc_jump.clone());
                    }))
                    .child(Icon::new(IconName::ExternalLink).xsmall())
                    .child(loc.short_label()),
            )
        })
}
