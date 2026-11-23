use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _,
    assets::IconName,
    base::SelectableText,
    component::{Icon, Selectable as _, Sizable as _, button::Button},
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::backend::BackendCommand;
use crate::protocol::{
    LayoutConstraints, LayoutExplorerNode, LayoutSize, WidgetProperty, WidgetTreeNode,
};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;
use crate::ui::components::{kv_row, pill_badge};

const MAIN_AXIS_OPTIONS: [&str; 6] = [
    "start",
    "center",
    "end",
    "spaceBetween",
    "spaceAround",
    "spaceEvenly",
];

const CROSS_AXIS_OPTIONS: [&str; 4] = ["start", "center", "end", "stretch"];

pub fn render_layout_explorer_subtab(
    view: &RootView,
    node: &WidgetTreeNode,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let layout = view
        .inspector
        .selected_layout_node
        .clone()
        .unwrap_or_else(|| synthesize_layout_from_properties(node, &view.inspector.selected_properties));

    let vid = layout.value_id.clone();
    let is_flex = layout.is_flex;
    let direction_label = layout
        .direction
        .as_deref()
        .unwrap_or(if is_flex { "horizontal" } else { "box" });
    let is_horizontal = direction_label.eq_ignore_ascii_case("horizontal");
    let current_main = layout
        .main_axis_alignment
        .clone()
        .unwrap_or_else(|| "start".to_string());
    let current_cross = layout
        .cross_axis_alignment
        .clone()
        .unwrap_or_else(|| "center".to_string());

    let size_text = match &layout.size {
        Some(sz) => format!("{:.1} × {:.1} px", sz.width, sz.height),
        None => "Unmeasured".to_string(),
    };
    let constraints_text = match &layout.constraints {
        Some(c) => c.description.clone(),
        None => "Unconstrained / Unknown".to_string(),
    };

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            // Visual Box & Constraints Schematic Card
            div()
                .p_3()
                .rounded(px(6.0))
                .bg(DevToolsTheme::bg_main())
                .border_1()
                .border_color(if layout.has_overflow {
                    DevToolsTheme::error_red()
                } else {
                    DevToolsTheme::border()
                })
                .flex()
                .flex_col()
                .gap_2p5()
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
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(DevToolsTheme::accent_blue())
                                        .child("Box Constraints & Computed Size"),
                                )
                                .child(pill_badge(
                                    if is_flex {
                                        if is_horizontal {
                                            "FLEX ROW"
                                        } else {
                                            "FLEX COLUMN"
                                        }
                                    } else {
                                        "RENDER BOX"
                                    },
                                    if is_flex {
                                        DevToolsTheme::accent_purple()
                                    } else {
                                        DevToolsTheme::accent_cyan()
                                    },
                                    0.18,
                                )),
                        )
                        .when(layout.has_overflow, |el| {
                            el.child(pill_badge(
                                "OVERFLOW DETECTED",
                                DevToolsTheme::error_red(),
                                0.22,
                            ))
                        }),
                )
                .child(
                    // Outer Constraints Box -> Inner Computed Size Box Diagram
                    div()
                        .w_full()
                        .p_3()
                        .rounded(px(6.0))
                        .bg(DevToolsTheme::bg_panel())
                        .border_1()
                        .border_color(DevToolsTheme::to_hsla(DevToolsTheme::accent_cyan(), 0.45))
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .text_color(DevToolsTheme::accent_cyan())
                                .child(SelectableText::new(
                                    SharedString::from(format!("le-constraints-{vid}")),
                                    format!("Constraints: {constraints_text}"),
                                )),
                        )
                        .child(
                            div()
                                .w_full()
                                .p_3()
                                .rounded(px(5.0))
                                .bg(DevToolsTheme::to_hsla(DevToolsTheme::accent_blue(), 0.14))
                                .border_1()
                                .border_color(if layout.has_overflow {
                                    DevToolsTheme::error_red()
                                } else {
                                    DevToolsTheme::accent_blue()
                                })
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
                                                .font_weight(gpui_kit::FontWeight::BOLD)
                                                .text_color(DevToolsTheme::text_primary())
                                                .child(layout.widget_runtime_type.clone()),
                                        )
                                        .child(
                                            div()
                                                .px_2()
                                                .py(px(1.0))
                                                .rounded(px(3.0))
                                                .bg(DevToolsTheme::bg_main())
                                                .text_color(DevToolsTheme::success_green())
                                                .child(SelectableText::new(
                                                    SharedString::from(format!("le-size-{vid}")),
                                                    format!("Size: {size_text}"),
                                                )),
                                        ),
                                )
                                .when(!layout.children.is_empty(), |el| {
                                    let children_for_boxes = layout.children.clone();
                                    el.child(
                                        div()
                                            .w_full()
                                            .pt_1()
                                            .flex()
                                            .when(!is_horizontal, |d| d.flex_col())
                                            .flex_wrap()
                                            .gap_1p5()
                                            .children(
                                                children_for_boxes.into_iter().enumerate().map(
                                                    |(idx, child)| {
                                                        let child_vid = child.value_id.clone();
                                                        let child_sz = match &child.size {
                                                            Some(s) => format!(
                                                                "{:.0}×{:.0}",
                                                                s.width, s.height
                                                            ),
                                                            None => "-".to_string(),
                                                        };
                                                        let flex_badge = child
                                                            .flex_factor
                                                            .filter(|&f| f > 0)
                                                            .map(|f| {
                                                                format!(
                                                                    "flex:{f} ({})",
                                                                    child
                                                                        .flex_fit
                                                                        .as_deref()
                                                                        .unwrap_or("tight")
                                                                )
                                                            });
                                                        div()
                                                            .id(SharedString::from(format!(
                                                                "le-box-child-{idx}-{child_vid}"
                                                            )))
                                                            .px_2()
                                                            .py_1p5()
                                                            .rounded(px(4.0))
                                                            .cursor_pointer()
                                                            .bg(DevToolsTheme::bg_main())
                                                            .border_1()
                                                            .border_color(DevToolsTheme::border())
                                                            .hover(|h| {
                                                                h.border_color(
                                                                    DevToolsTheme::accent_blue(),
                                                                )
                                                            })
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_2()
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    if !child_vid.is_empty() {
                                                                        let _ = this
                                                                            .inspector
                                                                            .select_node(
                                                                                &child_vid,
                                                                            );
                                                                        this.send_cmd(
                                                                            BackendCommand::SelectWidgetNode {
                                                                                value_id: child_vid
                                                                                    .clone(),
                                                                                highlight_on_device:
                                                                                    this.overlays
                                                                                        .inspector,
                                                                            },
                                                                        );
                                                                        cx.notify();
                                                                    }
                                                                },
                                                            ))
                                                            .child(
                                                                div()
                                                                    .text_color(
                                                                        DevToolsTheme::accent_blue(),
                                                                    )
                                                                    .font_weight(
                                                                        gpui_kit::FontWeight::SEMIBOLD,
                                                                    )
                                                                    .child(
                                                                        child
                                                                            .widget_runtime_type
                                                                            .clone(),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap_1p5()
                                                                    .when_some(
                                                                        flex_badge,
                                                                        |d, fb| {
                                                                            d.child(pill_badge(
                                                                                &fb,
                                                                                DevToolsTheme::accent_purple(),
                                                                                0.2,
                                                                            ))
                                                                        },
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_color(
                                                                                DevToolsTheme::text_secondary(),
                                                                            )
                                                                            .child(child_sz),
                                                                    ),
                                                            )
                                                    },
                                                ),
                                            ),
                                    )
                                }),
                        ),
                )
                .child(kv_row("Computed Size", &size_text))
                .child(kv_row("Constraints", &constraints_text))
                .when_some(layout.render_object_description.as_deref(), |el, ro| {
                    el.child(kv_row("RenderObject", ro))
                }),
        )
        .when(is_flex, |el| {
            let vid_main = vid.clone();
            let vid_cross = vid.clone();
            let cross_for_main = current_cross.clone();
            let main_for_cross = current_main.clone();
            el.child(
                div()
                    .p_3()
                    .rounded(px(6.0))
                    .bg(DevToolsTheme::bg_main())
                    .border_1()
                    .border_color(DevToolsTheme::border())
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(DevToolsTheme::accent_purple())
                                    .child("Interactive Flex Layout Controls (Live RPC)"),
                            )
                            .child(
                                div()
                                    .font_family(DevToolsTheme::MONO_FONT)
                                    .text_xs()
                                    .text_color(DevToolsTheme::text_secondary())
                                    .child(format!("direction: {direction_label}")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_family(DevToolsTheme::MONO_FONT)
                                    .text_xs()
                                    .text_color(DevToolsTheme::text_secondary())
                                    .child("MainAxisAlignment:"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_1()
                                    .children(MAIN_AXIS_OPTIONS.into_iter().map(|opt| {
                                        let active = current_main.eq_ignore_ascii_case(opt)
                                            || current_main.ends_with(&format!(".{opt}"));
                                        let vid_c = vid_main.clone();
                                        let cross_c = cross_for_main.clone();
                                        Button::new(SharedString::from(format!(
                                            "btn-flex-main-{opt}"
                                        )))
                                        .small()
                                        .compact()
                                        .selected(active)
                                        .label(opt)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !vid_c.is_empty() {
                                                if let Some(ref mut ln) =
                                                    this.inspector.selected_layout_node
                                                {
                                                    ln.main_axis_alignment = Some(opt.to_string());
                                                }
                                                this.send_cmd(BackendCommand::SetFlexProperties {
                                                    value_id: vid_c.clone(),
                                                    main_axis_alignment: opt.to_string(),
                                                    cross_axis_alignment: clean_enum_suffix(
                                                        &cross_c,
                                                    ),
                                                });
                                                cx.notify();
                                            }
                                        }))
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_family(DevToolsTheme::MONO_FONT)
                                    .text_xs()
                                    .text_color(DevToolsTheme::text_secondary())
                                    .child("CrossAxisAlignment:"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_1()
                                    .children(CROSS_AXIS_OPTIONS.into_iter().map(|opt| {
                                        let active = current_cross.eq_ignore_ascii_case(opt)
                                            || current_cross.ends_with(&format!(".{opt}"));
                                        let vid_c = vid_cross.clone();
                                        let main_c = main_for_cross.clone();
                                        Button::new(SharedString::from(format!(
                                            "btn-flex-cross-{opt}"
                                        )))
                                        .small()
                                        .compact()
                                        .selected(active)
                                        .label(opt)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !vid_c.is_empty() {
                                                if let Some(ref mut ln) =
                                                    this.inspector.selected_layout_node
                                                {
                                                    ln.cross_axis_alignment = Some(opt.to_string());
                                                }
                                                this.send_cmd(BackendCommand::SetFlexProperties {
                                                    value_id: vid_c.clone(),
                                                    main_axis_alignment: clean_enum_suffix(&main_c),
                                                    cross_axis_alignment: opt.to_string(),
                                                });
                                                cx.notify();
                                            }
                                        }))
                                    })),
                            ),
                    ),
            )
        })
        .when(
            layout.flex_factor.is_some()
                || layout.flex_fit.is_some()
                || matches!(
                    layout.widget_runtime_type.as_str(),
                    "Expanded" | "Flexible" | "Spacer"
                ),
            |el| {
                el.child(render_flex_child_controls_card(
                    "Selected Flex Child Controls",
                    &layout.value_id,
                    &layout.widget_runtime_type,
                    layout.flex_factor,
                    layout.flex_fit.as_deref(),
                    cx,
                ))
            },
        )
        .when(!layout.children.is_empty(), |el| {
            let children = layout.children.clone();
            el.child(
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
                            .child(format!(
                                "Child Layout Nodes ({})",
                                children.len()
                            )),
                    )
                    .children(children.into_iter().enumerate().map(|(idx, child)| {
                        render_child_flex_row(idx, &child, cx)
                    })),
            )
        })
}

fn render_flex_child_controls_card(
    title: &str,
    value_id: &str,
    widget_type: &str,
    flex_factor: Option<u32>,
    flex_fit: Option<&str>,
    cx: &mut Context<RootView>,
) -> impl IntoElement + use<> {
    let cur_factor = flex_factor.unwrap_or(1);
    let cur_fit = flex_fit.unwrap_or("tight").to_string();

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
                .text_color(DevToolsTheme::accent_cyan())
                .child(format!("{title} ({widget_type})")),
        )
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
                        .gap_1()
                        .child(
                            div()
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_xs()
                                .text_color(DevToolsTheme::text_secondary())
                                .mr_1()
                                .child("flexFactor:"),
                        )
                        .children([0u32, 1, 2, 3, 4].into_iter().map(|factor| {
                            let vid = value_id.to_string();
                            Button::new(SharedString::from(format!(
                                "btn-self-flex-factor-{vid}-{factor}"
                            )))
                            .small()
                            .compact()
                            .selected(cur_factor == factor)
                            .label(factor.to_string())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !vid.is_empty() {
                                    if let Some(ref mut ln) = this.inspector.selected_layout_node {
                                        ln.flex_factor = Some(factor);
                                    }
                                    this.send_cmd(BackendCommand::SetFlexFactor {
                                        value_id: vid.clone(),
                                        flex_factor: factor,
                                    });
                                    cx.notify();
                                }
                            }))
                        })),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .font_family(DevToolsTheme::MONO_FONT)
                                .text_xs()
                                .text_color(DevToolsTheme::text_secondary())
                                .mr_1()
                                .child("flexFit:"),
                        )
                        .children(["tight", "loose"].into_iter().map(|fit_opt| {
                            let vid = value_id.to_string();
                            let active = cur_fit.eq_ignore_ascii_case(fit_opt)
                                || cur_fit.ends_with(&format!(".{fit_opt}"));
                            Button::new(SharedString::from(format!(
                                "btn-self-flex-fit-{vid}-{fit_opt}"
                            )))
                            .small()
                            .compact()
                            .selected(active)
                            .label(fit_opt)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !vid.is_empty() {
                                    if let Some(ref mut ln) = this.inspector.selected_layout_node {
                                        ln.flex_fit = Some(fit_opt.to_string());
                                    }
                                    this.send_cmd(BackendCommand::SetFlexFit {
                                        value_id: vid.clone(),
                                        flex_fit: fit_opt.to_string(),
                                    });
                                    cx.notify();
                                }
                            }))
                        })),
                ),
        )
}

fn render_child_flex_row(
    idx: usize,
    child: &LayoutExplorerNode,
    cx: &mut Context<RootView>,
) -> impl IntoElement + use<> {
    let vid = child.value_id.clone();
    let cur_factor = child.flex_factor.unwrap_or(0);
    let cur_fit = child.flex_fit.clone().unwrap_or_else(|| "loose".to_string());
    let sz_label = match &child.size {
        Some(s) => format!("{:.1} × {:.1} px", s.width, s.height),
        None => "-".to_string(),
    };

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
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(DevToolsTheme::accent_blue())
                        .child(child.widget_runtime_type.clone()),
                )
                .child(
                    div()
                        .text_color(DevToolsTheme::text_secondary())
                        .child(sz_label),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_color(DevToolsTheme::text_muted())
                        .mr_1()
                        .child("flex:"),
                )
                .children([0u32, 1, 2, 3].into_iter().map(|factor| {
                    let child_id = vid.clone();
                    Button::new(SharedString::from(format!(
                        "btn-ch-flex-{idx}-{factor}"
                    )))
                    .small()
                    .compact()
                    .selected(cur_factor == factor)
                    .label(factor.to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !child_id.is_empty() {
                            if let Some(ref mut ln) = this.inspector.selected_layout_node
                                && let Some(ch) =
                                    ln.children.iter_mut().find(|c| c.value_id == child_id)
                            {
                                ch.flex_factor = Some(factor);
                            }
                            this.send_cmd(BackendCommand::SetFlexFactor {
                                value_id: child_id.clone(),
                                flex_factor: factor,
                            });
                            cx.notify();
                        }
                    }))
                }))
                .children(["tight", "loose"].into_iter().map(|fit_opt| {
                    let child_id = vid.clone();
                    let active = cur_fit.eq_ignore_ascii_case(fit_opt)
                        || cur_fit.ends_with(&format!(".{fit_opt}"));
                    Button::new(SharedString::from(format!("btn-ch-fit-{idx}-{fit_opt}")))
                        .small()
                        .compact()
                        .selected(active)
                        .label(fit_opt)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !child_id.is_empty() {
                                if let Some(ref mut ln) = this.inspector.selected_layout_node
                                    && let Some(ch) =
                                        ln.children.iter_mut().find(|c| c.value_id == child_id)
                                {
                                    ch.flex_fit = Some(fit_opt.to_string());
                                }
                                this.send_cmd(BackendCommand::SetFlexFit {
                                    value_id: child_id.clone(),
                                    flex_fit: fit_opt.to_string(),
                                });
                                cx.notify();
                            }
                        }))
                }))
                .child(
                    Button::new(SharedString::from(format!("btn-ch-inspect-{idx}")))
                        .small()
                        .compact()
                        .icon(Icon::new(IconName::ChevronRight))
                        .tooltip("Select child widget")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !vid.is_empty() {
                                let _ = this.inspector.select_node(&vid);
                                this.send_cmd(BackendCommand::SelectWidgetNode {
                                    value_id: vid.clone(),
                                    highlight_on_device: this.overlays.inspector,
                                });
                                cx.notify();
                            }
                        })),
                ),
        )
}

fn clean_enum_suffix(raw: &str) -> String {
    raw.rsplit('.').next().unwrap_or(raw).trim().to_string()
}

fn synthesize_layout_from_properties(
    node: &WidgetTreeNode,
    props: &[WidgetProperty],
) -> LayoutExplorerNode {
    let find_prop = |name: &str| -> Option<String> {
        props
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.value.clone())
    };

    let size = find_prop("size").and_then(|s| {
        // Parse e.g. `Size(360.0, 48.0)`
        let inner = s
            .trim_start_matches("Size")
            .trim_matches(|c| c == '(' || c == ')' || c == ' ');
        let (w_str, h_str) = inner.split_once(',')?;
        let w = w_str.trim().parse::<f64>().ok()?;
        let h = h_str.trim().parse::<f64>().ok()?;
        Some(LayoutSize {
            width: w,
            height: h,
        })
    });

    let constraints = find_prop("constraints").map(|desc| LayoutConstraints {
        constraint_type: "BoxConstraints".to_string(),
        description: desc,
        min_width: 0.0,
        max_width: None,
        min_height: 0.0,
        max_height: None,
    });

    let mut direction = find_prop("direction").map(|d| clean_enum_suffix(&d));
    if direction.is_none() {
        if node.widget_runtime_type == "Row" {
            direction = Some("horizontal".to_string());
        } else if node.widget_runtime_type == "Column" {
            direction = Some("vertical".to_string());
        }
    }

    let main_axis_alignment = find_prop("mainAxisAlignment").map(|v| clean_enum_suffix(&v));
    let cross_axis_alignment = find_prop("crossAxisAlignment").map(|v| clean_enum_suffix(&v));
    let main_axis_size = find_prop("mainAxisSize").map(|v| clean_enum_suffix(&v));
    let flex_factor = find_prop("flex").and_then(|v| v.trim().parse::<u32>().ok());
    let flex_fit = find_prop("fit").map(|v| clean_enum_suffix(&v));
    let is_flex = matches!(node.widget_runtime_type.as_str(), "Row" | "Column" | "Flex")
        || direction.is_some()
        || main_axis_alignment.is_some();

    let children: Vec<LayoutExplorerNode> = node
        .children
        .iter()
        .map(|ch| synthesize_layout_from_properties(ch, &ch.properties))
        .collect();

    LayoutExplorerNode {
        value_id: node.value_id.clone(),
        widget_runtime_type: node.widget_runtime_type.clone(),
        description: node.description.clone(),
        is_box: true,
        size,
        constraints,
        is_flex,
        flex_factor,
        flex_fit,
        direction,
        main_axis_alignment,
        cross_axis_alignment,
        main_axis_size,
        render_object_description: find_prop("renderObject"),
        has_overflow: false,
        children,
    }
}
