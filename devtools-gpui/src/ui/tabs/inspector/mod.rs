pub mod properties;
pub mod tree;

use flutter_debugger_protocol::ServiceOverlay;
use gpui_kit::{
    Context, Entity, IntoElement, ParentElement as _, Styled as _, UniformListScrollHandle, Window,
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
use crate::protocol::{SourceLocation, WidgetProperty, WidgetTreeNode};
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;
use crate::ui::components::subtoolbar_container;

use properties::render_widget_properties_pane;
use tree::render_widget_tree_list;

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
    pub auto_jump_to_source: bool,
    pub search_input: Entity<InputState>,
    pub search_query: String,
    pub flat_rows: Vec<FlatWidgetRow>,
    pub scroll_handle: UniformListScrollHandle,
}

impl InspectorTabState {
    pub fn new(search_input: Entity<InputState>) -> Self {
        Self {
            root: None,
            selected_value_id: None,
            selected_properties: Vec::new(),
            auto_jump_to_source: true,
            search_input,
            search_query: String::new(),
            flat_rows: Vec::new(),
            scroll_handle: UniformListScrollHandle::new(),
        }
    }

    pub fn set_search_query(&mut self, query: String) {
        if self.search_query != query {
            self.search_query = query;
            self.rebuild_flat_rows();
        }
    }

    pub fn set_root_tree(&mut self, root: WidgetTreeNode) -> Option<String> {
        let keep_existing = self
            .selected_value_id
            .as_deref()
            .and_then(|id| find_node_by_id(&root, id))
            .map(|n| (n.value_id.clone(), n.properties.clone()));

        let chosen_id = if let Some((id, props)) = keep_existing {
            if !props.is_empty() { self.selected_properties = props; }
            self.root = Some(root);
            Some(id)
        } else {
            let id = root.value_id.clone();
            self.selected_value_id = Some(id.clone());
            self.selected_properties = root.properties.clone();
            self.root = Some(root);
            Some(id)
        };
        self.rebuild_flat_rows();
        chosen_id
    }

    pub fn apply_children(&mut self, parent_value_id: &str, children: Vec<WidgetTreeNode>) {
        let mut children_opt = Some(children);
        if let Some(ref mut root) = self.root && attach_children_mut(root, parent_value_id, &mut children_opt) {
            self.rebuild_flat_rows();
        }
    }

    pub fn apply_device_selection(
        &mut self,
        tree: Option<WidgetTreeNode>,
        selected_node: WidgetTreeNode,
        properties: Vec<WidgetProperty>,
    ) {
        let target_id = selected_node.value_id.clone();
        if let Some(mut new_root) = tree {
            if let Some(ref old_root) = self.root {
                let mut expanded_ids = std::collections::HashSet::new();
                collect_expanded_ids(old_root, &mut expanded_ids);
                restore_expanded_ids_mut(&mut new_root, &expanded_ids);
            }
            self.root = Some(new_root);
        }
        if let Some(ref mut root) = self.root {
            expand_path_to_node_mut(root, &target_id);
        } else {
            self.root = Some(selected_node.clone());
        }
        self.selected_value_id = Some(target_id.clone());
        self.selected_properties = if !properties.is_empty() { properties } else { selected_node.properties };
        self.rebuild_flat_rows();
        if let Some(idx) = self.flat_rows.iter().position(|r| r.value_id == target_id) {
            self.scroll_handle.scroll_to_item(idx, gpui_kit::ScrollStrategy::Center);
        }
    }

    pub fn apply_properties(&mut self, value_id: &str, props: Vec<WidgetProperty>) {
        if self.selected_value_id.as_deref() == Some(value_id) && !props.is_empty() {
            self.selected_properties = props;
        }
    }

    pub fn select_node(&mut self, value_id: &str) -> Option<SourceLocation> {
        self.selected_value_id = Some(value_id.to_string());
        if let Some(ref root) = self.root && let Some(node) = find_node_by_id(root, value_id) {
            self.selected_properties = node.properties.clone();
            return node.location.clone();
        }
        None
    }

    pub fn toggle_expand(&mut self, value_id: &str) -> bool {
        let mut needs_lazy_load = false;
        if let Some(ref mut root) = self.root {
            toggle_node_expand_mut(root, value_id, &mut needs_lazy_load);
            self.rebuild_flat_rows();
        }
        needs_lazy_load
    }

    pub fn set_all_expanded(&mut self, expanded: bool) {
        if let Some(ref mut root) = self.root {
            set_all_expanded_mut(root, expanded);
            self.rebuild_flat_rows();
        }
    }

    pub fn selected_node(&self) -> Option<&WidgetTreeNode> {
        find_node_by_id(self.root.as_ref()?, self.selected_value_id.as_deref()?)
    }

    /// Resolves the best source file location to jump to in Zed for `selected`:
    /// 1. `selected.location` if present and non-empty
    /// 2. Matching node in `self.root` by `value_id` if it carries a `location`
    /// 3. Nearest ancestor in `self.root` with `created_by_local_project && location.is_some()`
    ///    (falling back to any ancestor with `location.is_some()`).
    pub fn resolve_jump_location(&self, selected: &WidgetTreeNode) -> Option<SourceLocation> {
        if let Some(ref loc) = selected.location
            && !loc.uri.trim().is_empty()
        {
            return Some(loc.clone());
        }

        let root = self.root.as_ref()?;
        if !selected.value_id.is_empty() {
            if let Some(in_tree) = find_node_by_id(root, &selected.value_id)
                && let Some(ref loc) = in_tree.location
                && !loc.uri.trim().is_empty()
            {
                return Some(loc.clone());
            }

            let mut chain: Vec<&WidgetTreeNode> = Vec::new();
            if collect_ancestor_chain(root, &selected.value_id, &mut chain) {
                for ancestor in chain.iter().rev() {
                    if ancestor.created_by_local_project
                        && let Some(ref loc) = ancestor.location
                        && !loc.uri.trim().is_empty()
                    {
                        return Some(loc.clone());
                    }
                }
                for ancestor in chain.iter().rev() {
                    if let Some(ref loc) = ancestor.location
                        && !loc.uri.trim().is_empty()
                    {
                        return Some(loc.clone());
                    }
                }
            }
        }

        None
    }

    pub fn flattened_count(&self) -> usize {
        fn count_nodes(n: &WidgetTreeNode) -> usize { 1 + n.children.iter().map(count_nodes).sum::<usize>() }
        self.root.as_ref().map_or(0, count_nodes)
    }

    pub fn rebuild_flat_rows(&mut self) {
        self.flat_rows.clear();
        let Some(ref root) = self.root else { return };
        let q = self.search_query.trim().to_ascii_lowercase();
        flatten_node_rec(root, 0, &q, &mut self.flat_rows);
    }
}

fn find_node_by_id<'a>(node: &'a WidgetTreeNode, value_id: &str) -> Option<&'a WidgetTreeNode> {
    if node.value_id == value_id { return Some(node); }
    node.children.iter().find_map(|c| find_node_by_id(c, value_id))
}

fn collect_ancestor_chain<'a>(
    node: &'a WidgetTreeNode,
    target_id: &str,
    chain: &mut Vec<&'a WidgetTreeNode>,
) -> bool {
    if node.value_id == target_id {
        return true;
    }
    chain.push(node);
    for child in &node.children {
        if collect_ancestor_chain(child, target_id, chain) {
            return true;
        }
    }
    chain.pop();
    false
}

fn collect_expanded_ids(node: &WidgetTreeNode, out: &mut std::collections::HashSet<String>) {
    if node.expanded && !node.value_id.is_empty() { out.insert(node.value_id.clone()); }
    for child in &node.children { collect_expanded_ids(child, out); }
}

fn restore_expanded_ids_mut(node: &mut WidgetTreeNode, expanded_ids: &std::collections::HashSet<String>) {
    if expanded_ids.contains(&node.value_id) { node.expanded = true; }
    for child in &mut node.children { restore_expanded_ids_mut(child, expanded_ids); }
}

fn attach_children_mut(node: &mut WidgetTreeNode, parent_id: &str, children: &mut Option<Vec<WidgetTreeNode>>) -> bool {
    if node.value_id == parent_id {
        node.children = children.take().unwrap_or_default();
        node.has_children = !node.children.is_empty();
        node.expanded = true;
        return true;
    }
    node.children.iter_mut().any(|c| attach_children_mut(c, parent_id, children))
}

fn expand_path_to_node_mut(node: &mut WidgetTreeNode, target_id: &str) -> bool {
    if node.value_id == target_id { return true; }
    for child in &mut node.children {
        if expand_path_to_node_mut(child, target_id) {
            node.expanded = true;
            return true;
        }
    }
    false
}

fn toggle_node_expand_mut(node: &mut WidgetTreeNode, value_id: &str, needs_lazy_load: &mut bool) -> bool {
    if node.value_id == value_id {
        node.expanded = !node.expanded;
        if node.expanded && node.has_children && node.children.is_empty() { *needs_lazy_load = true; }
        return true;
    }
    node.children.iter_mut().any(|c| toggle_node_expand_mut(c, value_id, needs_lazy_load))
}

fn set_all_expanded_mut(node: &mut WidgetTreeNode, expanded: bool) {
    node.expanded = expanded;
    for child in &mut node.children { set_all_expanded_mut(child, expanded); }
}

fn node_or_descendant_matches(node: &WidgetTreeNode, q: &str) -> bool {
    if q.is_empty() { return true; }
    node.widget_runtime_type.to_ascii_lowercase().contains(q)
        || node.description.to_ascii_lowercase().contains(q)
        || node.location.as_ref().is_some_and(|l| l.uri.to_ascii_lowercase().contains(q))
        || node.children.iter().any(|c| node_or_descendant_matches(c, q))
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
        let visible_count = self.inspector.flat_rows.len();
        let total_count = self.inspector.flattened_count();
        let select_mode_on = self.overlays.inspector;
        let auto_jump_on = self.inspector.auto_jump_to_source;
        let selected_loc = self
            .inspector
            .selected_node()
            .and_then(|n| self.inspector.resolve_jump_location(n));

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
                                Button::new("insp-refresh-btn")
                                    .small()
                                    .compact()
                                    .icon(Icon::new(IconName::RefreshCw))
                                    .label("Refresh Tree")
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
                                    .icon(Icon::new(IconName::Crosshair))
                                    .label(if select_mode_on {
                                        "Select Widget: ON"
                                    } else {
                                        "Select Widget: OFF"
                                    })
                                    .tooltip("Toggle device on-screen widget selection mode")
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.toggle_overlay(ServiceOverlay::Inspector);
                                    })),
                            )
                            .child(
                                Button::new("insp-auto-jump-btn")
                                    .small()
                                    .compact()
                                    .selected(auto_jump_on)
                                    .icon(Icon::new(IconName::MousePointerClick))
                                    .label(if auto_jump_on {
                                        "Auto-Jump: ON"
                                    } else {
                                        "Auto-Jump: OFF"
                                    })
                                    .tooltip("Automatically jump to Zed source location when a widget is selected on device")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.inspector.auto_jump_to_source =
                                            !this.inspector.auto_jump_to_source;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div().w(px(240.0)).child(
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
                                        "{visible_count} visible / {total_count} total widgets"
                                    )),
                            )
                            .child(
                                Button::new("insp-jump-source-btn")
                                    .small()
                                    .compact()
                                    .icon(Icon::new(IconName::ExternalLink))
                                    .label("Jump to Source")
                                    .tooltip(
                                        "Open selected Widget creation location in Zed Editor",
                                    )
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
            .child(
                div().flex_1().min_h_0().w_full().child(
                    h_resizable("inspector-split-pane")
                        .child(
                            resizable_panel()
                                .size(px(640.0))
                                .size_range(px(320.0)..px(1050.0))
                                .child(render_widget_tree_list(self, cx)),
                        )
                        .child(resizable_panel().child(render_widget_properties_pane(self, cx))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn device_selection_expands_to_nested_target_without_forging_root_child() {
        let mut tree = WidgetTreeNode::parse_from_json(&json!({
            "result": {
                "valueId": "inspector-root", "widgetRuntimeType": "MaterialApp", "description": "MaterialApp",
                "children": [{
                    "valueId": "inspector-scaffold", "widgetRuntimeType": "Scaffold", "description": "Scaffold",
                    "children": [{ "valueId": "inspector-btn", "widgetRuntimeType": "FilledButton", "description": "FilledButton" }]
                }]
            }
        })).unwrap();
        set_all_expanded_mut(&mut tree, false);
        assert!(expand_path_to_node_mut(&mut tree, "inspector-btn"));
        assert!(tree.expanded && tree.children[0].expanded);
        assert_eq!(tree.children.len(), 1);
    }

    #[test]
    fn resolves_jump_location_from_selected_node_or_tree_or_nearest_local_ancestor() {
        let root = WidgetTreeNode::parse_from_json(&json!({
            "result": {
                "valueId": "inspector-root",
                "widgetRuntimeType": "HomePage",
                "description": "HomePage",
                "createdByLocalProject": true,
                "creationLocation": {
                    "file": "file:///Users/dev/app/lib/home.dart",
                    "line": 18,
                    "column": 5
                },
                "children": [{
                    "valueId": "inspector-inner-padding",
                    "widgetRuntimeType": "Padding",
                    "description": "Padding",
                    "createdByLocalProject": false,
                    "children": [{
                        "valueId": "inspector-leaf-text",
                        "widgetRuntimeType": "RichText",
                        "description": "RichText",
                        "createdByLocalProject": false
                    }]
                }]
            }
        }))
        .expect("root");

        // Case 1: Selected node has no creationLocation of its own -> walks up to HomePage ancestor
        let leaf_without_loc = WidgetTreeNode {
            value_id: "inspector-leaf-text".into(),
            widget_runtime_type: "RichText".into(),
            description: "RichText".into(),
            has_children: false,
            created_by_local_project: false,
            location: None,
            properties: Vec::new(),
            children: Vec::new(),
            expanded: false,
        };
        let mut chain = Vec::new();
        assert!(collect_ancestor_chain(
            &root,
            &leaf_without_loc.value_id,
            &mut chain
        ));
        assert_eq!(chain.len(), 2);
        let ancestor_loc = chain
            .iter()
            .rev()
            .find(|n| n.created_by_local_project && n.location.is_some())
            .and_then(|n| n.location.clone())
            .expect("ancestor loc");
        assert_eq!(ancestor_loc.uri, "file:///Users/dev/app/lib/home.dart");
        assert_eq!(ancestor_loc.line, 18);
    }
}
