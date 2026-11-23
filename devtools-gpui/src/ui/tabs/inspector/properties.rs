use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
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

use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;
use crate::ui::components::empty_placeholder;

pub fn render_widget_properties_pane(
    view: &RootView,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let Some(node) = view.inspector.selected_node().cloned() else {
        return empty_placeholder(
            "Select a Widget node to inspect its properties and source location",
        )
        .into_any_element();
    };
    let properties = view.inspector.selected_properties.clone();

    div()
        .id("inspector-props-scroll")
        .size_full()
        .bg(DevToolsTheme::bg_panel())
        .p_3()
        .flex()
        .flex_col()
        .gap_3()
        .overflow_y_scrollbar()
        .child(
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
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .text_color(DevToolsTheme::accent_blue())
                                .child(node.widget_runtime_type.clone()),
                        )
                        .when(node.created_by_local_project, |el| {
                            el.child(
                                div()
                                    .px_1p5()
                                    .py(px(1.0))
                                    .rounded(px(3.0))
                                    .bg(DevToolsTheme::to_hsla(
                                        DevToolsTheme::success_green(),
                                        0.18,
                                    ))
                                    .font_family(DevToolsTheme::MONO_FONT)
                                    .text_xs()
                                    .text_color(DevToolsTheme::success_green())
                                    .child("LOCAL PROJECT"),
                            )
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
                                "Description: {} · ID: {}",
                                node.description,
                                if node.value_id.is_empty() {
                                    "-"
                                } else {
                                    &node.value_id
                                }
                            ),
                        )),
                )
                .when_some(view.inspector.resolve_jump_location(&node), |el, loc| {
                    let loc_click = loc.clone();
                    let loc_str = format!("{}:{}:{}", loc.uri, loc.line, loc.column);
                    el.child(
                        div()
                            .mt_1()
                            .pt_2()
                            .border_t_1()
                            .border_color(DevToolsTheme::border())
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
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(DevToolsTheme::text_secondary())
                        .child(format!("Widget Properties ({})", properties.len())),
                )
                .when(properties.is_empty(), |el| {
                    el.child(
                        div()
                            .py_4()
                            .text_xs()
                            .text_color(DevToolsTheme::text_muted())
                            .child("No diagnostic properties reported for this widget."),
                    )
                })
                .children(properties.into_iter().enumerate().map(|(prop_idx, prop)| {
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
                                .w(px(140.0))
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
                })),
        )
        .into_any_element()
}
