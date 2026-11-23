use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _,
    assets::IconName,
    base::SelectableText,
    component::{
        Icon, Sizable as _,
        button::Button,
        scroll::ScrollableElement as _,
    },
    div,
    prelude::FluentBuilder as _,
    px, rgb,
};

use crate::protocol::{WidgetProperty, WidgetTreeNode};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;
use crate::ui::components::{empty_placeholder, pill_badge};
use crate::ui::tabs::inspector::{
    InspectorDetailSubTab, layout_explorer::render_layout_explorer_subtab,
};

pub fn render_widget_properties_pane(
    view: &RootView,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let Some(node) = view.inspector.selected_node().cloned() else {
        return empty_placeholder(
            "Select a Widget node to inspect its Layout Explorer, Properties, and Details Tree",
        )
        .into_any_element();
    };
    let active_tab = view.inspector.detail_sub_tab;
    let props_count = view.inspector.selected_properties.len();

    div()
        .size_full()
        .bg(DevToolsTheme::bg_panel())
        .flex()
        .flex_col()
        .child(render_selected_widget_header(view, &node, cx))
        .child(render_inspector_detail_subtab_bar(
            active_tab,
            props_count,
            cx,
        ))
        .child(
            div()
                .id("inspector-props-scroll")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_3()
                .overflow_y_scrollbar()
                .child(match active_tab {
                    InspectorDetailSubTab::LayoutExplorer => {
                        render_layout_explorer_subtab(view, &node, cx).into_any_element()
                    }
                    InspectorDetailSubTab::Properties => {
                        render_properties_subtab(view, &node, cx).into_any_element()
                    }
                    InspectorDetailSubTab::DetailsTree => {
                        render_details_tree_subtab(view, &node, cx).into_any_element()
                    }
                }),
        )
        .into_any_element()
}

fn render_selected_widget_header(
    view: &RootView,
    node: &WidgetTreeNode,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    div()
        .w_full()
        .px_3()
        .py_2()
        .bg(DevToolsTheme::bg_main())
        .border_b_1()
        .border_color(DevToolsTheme::border())
        .flex()
        .flex_col()
        .gap_1p5()
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
                        .child(
                            div()
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .text_color(DevToolsTheme::accent_blue())
                                .child(node.widget_runtime_type.clone()),
                        )
                        .when(node.created_by_local_project, |el| {
                            el.child(pill_badge(
                                "LOCAL PROJECT",
                                DevToolsTheme::success_green(),
                                0.18,
                            ))
                        }),
                )
                .child(
                    div()
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .text_color(DevToolsTheme::text_secondary())
                        .child(SelectableText::new(
                            SharedString::from(format!("prop-desc-{}", node.value_id)),
                            format!(
                                "{} · ID: {}",
                                node.description,
                                if node.value_id.is_empty() {
                                    "-"
                                } else {
                                    &node.value_id
                                }
                            ),
                        )),
                ),
        )
        .when_some(view.inspector.resolve_jump_location(node), |el, loc| {
            let loc_click = loc.clone();
            let loc_str = format!("{}:{}:{}", loc.uri, loc.line, loc.column);
            el.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(DevToolsTheme::MONO_FONT)
                            .text_xs()
                            .text_color(DevToolsTheme::accent_cyan())
                            .child(SelectableText::new(
                                SharedString::from(format!("prop-loc-{}", node.value_id)),
                                loc_str,
                            )),
                    )
                    .child(
                        Button::new("btn-open-widget-in-zed")
                            .small()
                            .compact()
                            .icon(Icon::new(IconName::ExternalLink))
                            .label("Open in Zed")
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.jump_to_zed(loc_click.clone());
                            })),
                    ),
            )
        })
}

fn render_inspector_detail_subtab_bar(
    active_tab: InspectorDetailSubTab,
    props_count: usize,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    div()
        .w_full()
        .h(px(32.0))
        .px_3()
        .bg(DevToolsTheme::bg_toolbar())
        .border_b_1()
        .border_color(DevToolsTheme::border())
        .flex()
        .items_center()
        .gap_1()
        .children(InspectorDetailSubTab::all().into_iter().map(|tab| {
            let is_active = tab == active_tab;
            let label = match tab {
                InspectorDetailSubTab::LayoutExplorer => "Layout Explorer".to_string(),
                InspectorDetailSubTab::Properties => format!("Properties ({props_count})"),
                InspectorDetailSubTab::DetailsTree => "Details Tree".to_string(),
            };
            div()
                .id(SharedString::from(format!(
                    "insp-detail-tab-{}",
                    tab.label()
                )))
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
                    this.inspector.detail_sub_tab = tab;
                    cx.notify();
                }))
                .child(label)
        }))
}

fn render_properties_subtab(
    view: &RootView,
    node: &WidgetTreeNode,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let all_props = view.inspector.selected_properties.clone();
    let show_fine = view.inspector.show_all_properties;
    let fine_count = all_props
        .iter()
        .filter(|p| matches!(p.level.as_str(), "fine" | "debug"))
        .count();

    let visible_props: Vec<WidgetProperty> = all_props
        .into_iter()
        .filter(|p| show_fine || !matches!(p.level.as_str(), "fine" | "debug"))
        .collect();

    div()
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
                        .text_color(DevToolsTheme::text_secondary())
                        .child(format!("Diagnostic Properties ({})", visible_props.len())),
                )
                .when(fine_count > 0, |el| {
                    el.child(
                        Button::new("btn-toggle-fine-props")
                            .small()
                            .compact()
                            .label(if show_fine {
                                "Hide Fine/Default"
                            } else {
                                "Show All Properties"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector.show_all_properties =
                                    !this.inspector.show_all_properties;
                                cx.notify();
                            })),
                    )
                }),
        )
        .when(visible_props.is_empty(), |el| {
            el.child(
                div()
                    .py_4()
                    .text_xs()
                    .text_color(DevToolsTheme::text_muted())
                    .child("No diagnostic properties reported for this widget."),
            )
        })
        .children(visible_props.into_iter().enumerate().map(|(prop_idx, prop)| {
            let vid = &node.value_id;
            let key_id = SharedString::from(format!("wprop-k-{vid}-{prop_idx}"));
            let val_id = SharedString::from(format!("wprop-v-{vid}-{prop_idx}-{}", prop.value));
            div()
                .w_full()
                .py_1()
                .border_b_1()
                .border_color(DevToolsTheme::border())
                .font_family(DevToolsTheme::MONO_FONT)
                .text_xs()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(148.0))
                        .flex_shrink_0()
                        .text_color(DevToolsTheme::text_secondary())
                        .child(SelectableText::new(key_id, prop.name)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when_some(prop.color_argb, |el, argb| {
                            let alpha = ((argb >> 24) & 0xFF) as f32 / 255.0;
                            let swatch = DevToolsTheme::to_hsla(rgb(argb & 0x00FF_FFFF), alpha);
                            el.child(
                                div()
                                    .w(px(12.0))
                                    .h(px(12.0))
                                    .rounded(px(2.0))
                                    .border_1()
                                    .border_color(DevToolsTheme::border())
                                    .bg(swatch),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(DevToolsTheme::text_primary())
                                .child(SelectableText::new(val_id, prop.value)),
                        ),
                )
        }))
}

fn render_details_tree_subtab(
    view: &RootView,
    fallback_node: &WidgetTreeNode,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let root = view
        .inspector
        .selected_details_subtree
        .as_ref()
        .unwrap_or(fallback_node);
    let mut rows: Vec<(usize, String, WidgetTreeNode)> = Vec::new();
    collect_details_rows(
        root,
        0,
        "root",
        &view.inspector.collapsed_details_ids,
        &mut rows,
    );

    div()
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
                        .child(
                            "Widget Details Subtree (Full Diagnostic & RenderObject Hierarchy)",
                        ),
                )
                .child(
                    Button::new("btn-details-expand-all")
                        .small()
                        .compact()
                        .label("Expand All")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.inspector.collapsed_details_ids.clear();
                            cx.notify();
                        })),
                ),
        )
        .children(rows.into_iter().enumerate().map(|(idx, (depth, key, dnode))| {
            let is_collapsed = view.inspector.collapsed_details_ids.contains(&key);
            let has_expandable = !dnode.children.is_empty() || !dnode.properties.is_empty();
            let toggle_key = key.clone();
            let indent = (depth as f32) * 14.0;

            div()
                .w_full()
                .pl(px(indent))
                .py_1()
                .border_b_1()
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
                        .gap_1p5()
                        .child(
                            div()
                                .id(SharedString::from(format!("dt-toggle-{idx}")))
                                .w(px(16.0))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(DevToolsTheme::text_secondary())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.inspector.collapsed_details_ids.contains(&toggle_key) {
                                        this.inspector.collapsed_details_ids.remove(&toggle_key);
                                    } else {
                                        this.inspector
                                            .collapsed_details_ids
                                            .insert(toggle_key.clone());
                                    }
                                    cx.notify();
                                }))
                                .when(has_expandable, |el| {
                                    el.child(
                                        Icon::new(if is_collapsed {
                                            IconName::ChevronRight
                                        } else {
                                            IconName::ChevronDown
                                        })
                                        .xsmall(),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(DevToolsTheme::accent_blue())
                                .child(SelectableText::new(
                                    SharedString::from(format!("dt-type-{idx}-{key}")),
                                    dnode.widget_runtime_type.clone(),
                                )),
                        )
                        .when(
                            dnode.description != dnode.widget_runtime_type
                                && !dnode.description.is_empty(),
                            |el| {
                                el.child(
                                    div()
                                        .text_color(DevToolsTheme::text_secondary())
                                        .child(SelectableText::new(
                                            SharedString::from(format!("dt-desc-{idx}-{key}")),
                                            dnode.description.clone(),
                                        )),
                                )
                            },
                        ),
                )
                .when(!is_collapsed && !dnode.properties.is_empty(), |el| {
                    el.child(
                        div()
                            .pl(px(22.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .children(dnode.properties.into_iter().enumerate().map(
                                |(pidx, prop)| {
                                    div()
                                        .flex()
                                        .items_start()
                                        .gap_1p5()
                                        .text_color(DevToolsTheme::text_secondary())
                                        .child(SelectableText::new(
                                            SharedString::from(format!(
                                                "dt-prop-{idx}-{pidx}-{key}"
                                            )),
                                            format!("{}: {}", prop.name, prop.value),
                                        ))
                                },
                            )),
                    )
                })
        }))
}

fn collect_details_rows(
    node: &WidgetTreeNode,
    depth: usize,
    key: &str,
    collapsed: &std::collections::HashSet<String>,
    out: &mut Vec<(usize, String, WidgetTreeNode)>,
) {
    out.push((depth, key.to_string(), node.clone()));
    if collapsed.contains(key) {
        return;
    }
    for (idx, child) in node.children.iter().enumerate() {
        let child_key = if !child.value_id.is_empty() {
            format!("{key}/{}", child.value_id)
        } else {
            format!("{key}/{idx}")
        };
        collect_details_rows(child, depth + 1, &child_key, collapsed, out);
    }
}
