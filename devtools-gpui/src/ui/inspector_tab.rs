use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Rgba, SharedString,
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
    px, rgb,
};

use crate::backend::{
    BackendCommand, ServiceOverlay, SourceLocation, WidgetProperty, WidgetTreeNode,
};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;

#[derive(Debug, Clone)]
pub struct FlatWidgetRow {
    pub depth: usize,
    pub value_id: String,
    pub widget_runtime_type: String,
    pub description: String,
    pub has_children: bool,
    pub expanded: bool,
    pub created_by_local_project: bool,
    pub location: Option<SourceLocation>,
}

pub struct InspectorTabState {
    pub root: Option<WidgetTreeNode>,
    pub selected_value_id: Option<String>,
    pub selected_properties: Vec<WidgetProperty>,
    pub search_input: Entity<InputState>,
}

impl InspectorTabState {
    pub fn new(search_input: Entity<InputState>) -> Self {
        Self {
            root: None,
            selected_value_id: None,
            selected_properties: Vec::new(),
            search_input,
        }
    }

    pub fn set_root_tree(&mut self, root: WidgetTreeNode) -> Option<String> {
        let keep_existing = self
            .selected_value_id
            .as_deref()
            .and_then(|id| find_node_by_id(&root, id))
            .map(|n| (n.value_id.clone(), n.properties.clone()));

        if let Some((id, props)) = keep_existing {
            if !props.is_empty() {
                self.selected_properties = props;
            }
            self.root = Some(root);
            Some(id)
        } else {
            let id = root.value_id.clone();
            self.selected_value_id = Some(id.clone());
            self.selected_properties = root.properties.clone();
            self.root = Some(root);
            Some(id)
        }
    }

    pub fn apply_properties(&mut self, value_id: &str, props: Vec<WidgetProperty>) {
        if self.selected_value_id.as_deref() == Some(value_id) && !props.is_empty() {
            self.selected_properties = props;
        }
    }

    pub fn select_node(&mut self, value_id: &str) -> Option<SourceLocation> {
        self.selected_value_id = Some(value_id.to_string());
        if let Some(ref root) = self.root
            && let Some(node) = find_node_by_id(root, value_id)
        {
            self.selected_properties = node.properties.clone();
            return node.location.clone();
        }
        None
    }

    pub fn toggle_expand(&mut self, value_id: &str) {
        if let Some(ref mut root) = self.root {
            toggle_node_expand_mut(root, value_id);
        }
    }

    pub fn set_all_expanded(&mut self, expanded: bool) {
        if let Some(ref mut root) = self.root {
            set_all_expanded_mut(root, expanded);
        }
    }

    pub fn selected_node(&self) -> Option<&WidgetTreeNode> {
        let id = self.selected_value_id.as_deref()?;
        let root = self.root.as_ref()?;
        find_node_by_id(root, id)
    }

    pub fn flattened_count(&self) -> usize {
        fn count_nodes(n: &WidgetTreeNode) -> usize {
            1 + n.children.iter().map(count_nodes).sum::<usize>()
        }
        self.root.as_ref().map_or(0, count_nodes)
    }

    pub fn flatten_visible_rows(&self, query: &str) -> Vec<FlatWidgetRow> {
        let Some(ref root) = self.root else {
            return Vec::new();
        };
        let q = query.trim().to_ascii_lowercase();
        let mut out = Vec::new();
        flatten_node_rec(root, 0, &q, &mut out);
        out
    }
}

fn find_node_by_id<'a>(node: &'a WidgetTreeNode, value_id: &str) -> Option<&'a WidgetTreeNode> {
    if node.value_id == value_id {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_by_id(child, value_id) {
            return Some(found);
        }
    }
    None
}

fn toggle_node_expand_mut(node: &mut WidgetTreeNode, value_id: &str) -> bool {
    if node.value_id == value_id {
        node.expanded = !node.expanded;
        return true;
    }
    for child in &mut node.children {
        if toggle_node_expand_mut(child, value_id) {
            return true;
        }
    }
    false
}

fn set_all_expanded_mut(node: &mut WidgetTreeNode, expanded: bool) {
    node.expanded = expanded;
    for child in &mut node.children {
        set_all_expanded_mut(child, expanded);
    }
}

fn node_or_descendant_matches(node: &WidgetTreeNode, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    if node.widget_runtime_type.to_ascii_lowercase().contains(q)
        || node.description.to_ascii_lowercase().contains(q)
        || node
            .location
            .as_ref()
            .is_some_and(|l| l.uri.to_ascii_lowercase().contains(q))
    {
        return true;
    }
    node.children
        .iter()
        .any(|c| node_or_descendant_matches(c, q))
}

fn flatten_node_rec(
    node: &WidgetTreeNode,
    depth: usize,
    q: &str,
    out: &mut Vec<FlatWidgetRow>,
) {
    if !node_or_descendant_matches(node, q) {
        return;
    }
    out.push(FlatWidgetRow {
        depth,
        value_id: node.value_id.clone(),
        widget_runtime_type: node.widget_runtime_type.clone(),
        description: node.description.clone(),
        has_children: !node.children.is_empty() || node.has_children,
        expanded: node.expanded || !q.is_empty(),
        created_by_local_project: node.created_by_local_project,
        location: node.location.clone(),
    });

    if node.expanded || !q.is_empty() {
        for child in &node.children {
            flatten_node_rec(child, depth + 1, q, out);
        }
    }
}

impl RootView {
    pub fn render_inspector_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.inspector.search_input.read(cx).value().to_string();
        let rows = self.inspector.flatten_visible_rows(&query);
        let selected_node = self.inspector.selected_node().cloned();
        let selected_props = self.inspector.selected_properties.clone();
        let select_mode_on = self.overlays.inspector;
        let selected_loc = selected_node.as_ref().and_then(|n| n.location.clone());

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
                                Button::new("insp-refresh-btn")
                                    .small()
                                    .compact()
                                    .label("↻ Refresh Tree")
                                    .tooltip("Fetch latest Flutter Widget Summary Tree")
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.send_cmd(BackendCommand::RefreshWidgetTree);
                                    })),
                            )
                            .child(
                                Button::new("insp-select-mode-btn")
                                    .small()
                                    .compact()
                                    .selected(select_mode_on)
                                    .label(if select_mode_on {
                                        "🎯 Select Widget: ON"
                                    } else {
                                        "🎯 Select Widget: OFF"
                                    })
                                    .tooltip("Toggle device on-screen widget selection mode")
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.toggle_overlay(ServiceOverlay::Inspector);
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(240.0))
                                    .child(
                                        Input::new(&self.inspector.search_input)
                                            .id("insp-search-input")
                                            .small()
                                            .cleanable(true),
                                    ),
                            )
                            .child(
                                Button::new("insp-expand-all-btn")
                                    .small()
                                    .compact()
                                    .label("Expand All")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.inspector.set_all_expanded(true);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("insp-collapse-all-btn")
                                    .small()
                                    .compact()
                                    .label("Collapse All")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.inspector.set_all_expanded(false);
                                        cx.notify();
                                    })),
                            ),
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
                                        "{} visible / {} total widgets",
                                        rows.len(),
                                        self.inspector.flattened_count()
                                    )),
                            )
                            .child(
                                Button::new("insp-jump-source-btn")
                                    .small()
                                    .compact()
                                    .label("↗ Jump to Source")
                                    .tooltip("Open selected Widget creation location in Zed Editor")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(ref loc) = selected_loc {
                                            this.jump_to_zed(loc.clone());
                                        } else {
                                            this.show_toast(
                                                "Selected widget has no local source location",
                                                true,
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                    ),
            )
            // Left Widget Tree + Right Properties Pane
            .child(
                div().flex_1().min_h_0().w_full().child(
                    h_resizable("inspector-split-pane")
                        .child(
                            resizable_panel()
                                .size(px(640.0))
                                .size_range(px(320.0)..px(1050.0))
                                .child(render_widget_tree_list(
                                    rows,
                                    self.inspector.selected_value_id.as_deref(),
                                    cx,
                                )),
                        )
                        .child(resizable_panel().child(render_widget_properties_pane(
                            selected_node,
                            selected_props,
                            cx,
                        ))),
                ),
            )
    }
}

fn render_widget_tree_list(
    rows: Vec<FlatWidgetRow>,
    selected_id: Option<&str>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    div()
        .id("inspector-tree-scroll")
        .size_full()
        .bg(DevToolsTheme::bg_main())
        .flex()
        .flex_col()
        .overflow_y_scrollbar()
        .when(rows.is_empty(), |el| {
            el.child(
                div()
                    .w_full()
                    .py_12()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_xs()
                    .text_color(DevToolsTheme::text_muted())
                    .child("No Widget tree loaded yet.")
                    .child("Click '↻ Refresh Tree' once your Flutter application is running."),
            )
        })
        .children(rows.into_iter().enumerate().map(|(idx, row)| {
            let is_selected =
                !row.value_id.is_empty() && selected_id == Some(row.value_id.as_str());
            let vid = row.value_id.clone();
            let vid_toggle = row.value_id.clone();
            let indent_px = (row.depth as f32) * 16.0 + 8.0;
            let arrow = if row.has_children {
                if row.expanded { "▾" } else { "▸" }
            } else {
                "·"
            };

            div()
                .id(SharedString::from(format!("widget-row-{idx}-{}", row.value_id)))
                .w_full()
                .h(px(28.0))
                .pl(px(indent_px))
                .pr_2()
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = this.inspector.select_node(&vid);
                    if !vid.is_empty() {
                        this.send_cmd(BackendCommand::SelectWidgetNode {
                            value_id: vid.clone(),
                        });
                    }
                    cx.notify();
                }))
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
                                .text_color(DevToolsTheme::text_secondary())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.inspector.toggle_expand(&vid_toggle);
                                    cx.notify();
                                }))
                                .child(arrow),
                        )
                        .child(
                            div()
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
                            row.description != row.widget_runtime_type
                                && !row.description.is_empty(),
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
                .when_some(row.location, |el, loc| {
                    let loc_jump = loc.clone();
                    el.child(
                        div()
                            .id(SharedString::from(format!("widget-loc-{idx}")))
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
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.jump_to_zed(loc_jump.clone());
                            }))
                            .child(format!("↗ {}", loc.short_label())),
                    )
                })
        }))
}

fn render_widget_properties_pane(
    selected_node: Option<WidgetTreeNode>,
    properties: Vec<WidgetProperty>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let Some(node) = selected_node else {
        return div()
            .size_full()
            .bg(DevToolsTheme::bg_panel())
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(DevToolsTheme::text_muted())
            .child("Select a Widget node to inspect its properties and source location")
            .into_any_element();
    };

    div()
        .id("inspector-props-scroll")
        .size_full()
        .bg(DevToolsTheme::bg_panel())
        .p_3()
        .flex()
        .flex_col()
        .gap_3()
        .overflow_y_scrollbar()
        // Widget Header Card
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
                        .child(format!(
                            "Description: {} · ID: {}",
                            node.description,
                            if node.value_id.is_empty() {
                                "-"
                            } else {
                                &node.value_id
                            }
                        )),
                )
                .when_some(node.location, |el, loc| {
                    let loc_click = loc.clone();
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
                                    .font_family(DevToolsTheme::MONO_FONT)
                                    .text_xs()
                                    .text_color(DevToolsTheme::accent_cyan())
                                    .truncate()
                                    .child(format!("{}:{}:{}", loc.uri, loc.line, loc.column)),
                            )
                            .child(
                                Button::new("btn-open-widget-in-zed")
                                    .small()
                                    .compact()
                                    .label("↗ Open in Zed")
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.jump_to_zed(loc_click.clone());
                                    })),
                            ),
                    )
                }),
        )
        // Properties Table Card
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
                .children(properties.into_iter().map(|prop| {
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
                                .child(prop.name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when_some(prop.color_argb, |el, argb| {
                                    let rgb_hex = argb & 0x00FF_FFFF;
                                    let swatch: Rgba = rgb(rgb_hex);
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
                                        .text_color(DevToolsTheme::text_primary())
                                        .child(prop.value),
                                ),
                        )
                })),
        )
        .into_any_element()
}
