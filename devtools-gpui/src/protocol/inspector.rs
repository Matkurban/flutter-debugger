use std::path::Path;

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    pub uri: String,
    pub line: u32,
    pub column: u32,
}

impl SourceLocation {
    pub fn short_label(&self) -> String {
        let file_name = self.uri.rsplit('/').next().unwrap_or(&self.uri);
        format!("{file_name}:{}", self.line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetProperty {
    pub name: String,
    pub value: String,
    pub level: String,
    pub property_type: Option<String>,
    pub color_argb: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetTreeNode {
    pub value_id: String,
    pub widget_runtime_type: String,
    pub description: String,
    pub has_children: bool,
    pub created_by_local_project: bool,
    pub location: Option<SourceLocation>,
    pub properties: Vec<WidgetProperty>,
    pub children: Vec<WidgetTreeNode>,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutSize {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutConstraints {
    pub constraint_type: String,
    pub description: String,
    pub min_width: f64,
    pub max_width: Option<f64>,
    pub min_height: f64,
    pub max_height: Option<f64>,
}

impl LayoutConstraints {
    pub fn format_width_range(&self) -> String {
        match self.max_width {
            Some(max) if (self.min_width - max).abs() < 0.05 => format!("w={max:.1}"),
            Some(max) => format!("{:.1}<=w<={max:.1}", self.min_width),
            None => format!("{:.1}<=w<=Infinity", self.min_width),
        }
    }

    pub fn format_height_range(&self) -> String {
        match self.max_height {
            Some(max) if (self.min_height - max).abs() < 0.05 => format!("h={max:.1}"),
            Some(max) => format!("{:.1}<=h<={max:.1}", self.min_height),
            None => format!("{:.1}<=h<=Infinity", self.min_height),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutExplorerNode {
    pub value_id: String,
    pub widget_runtime_type: String,
    pub description: String,
    pub is_box: bool,
    pub size: Option<LayoutSize>,
    pub constraints: Option<LayoutConstraints>,
    pub is_flex: bool,
    pub flex_factor: Option<u32>,
    pub flex_fit: Option<String>,
    pub direction: Option<String>,
    pub main_axis_alignment: Option<String>,
    pub cross_axis_alignment: Option<String>,
    pub main_axis_size: Option<String>,
    pub render_object_description: Option<String>,
    pub has_overflow: bool,
    pub children: Vec<LayoutExplorerNode>,
}

impl LayoutExplorerNode {
    pub fn parse_from_json(v: &Value) -> Option<Self> {
        let root = v.get("result").filter(|r| !r.is_null()).unwrap_or(v);
        if root.is_null() || !root.is_object() {
            return None;
        }
        Self::parse_node(root)
    }

    fn parse_node(node: &Value) -> Option<Self> {
        if node.is_null() || !node.is_object() {
            return None;
        }
        let description = node
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("Widget")
            .to_string();
        let widget_runtime_type = node
            .get("widgetRuntimeType")
            .and_then(Value::as_str)
            .unwrap_or(&description)
            .to_string();
        let value_id = node
            .get("valueId")
            .and_then(Value::as_str)
            .or_else(|| node.get("objectId").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();

        let size = node.get("size").and_then(|s| {
            let w = parse_f64_field(s.get("width"))?;
            let h = parse_f64_field(s.get("height"))?;
            Some(LayoutSize {
                width: w,
                height: h,
            })
        });

        let constraints = node.get("constraints").and_then(|c| {
            if !c.is_object() {
                return None;
            }
            let constraint_type = c
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("Constraints")
                .to_string();
            let min_width = parse_f64_field(c.get("minWidth")).unwrap_or(0.0);
            let max_width = parse_f64_field(c.get("maxWidth")).filter(|v| v.is_finite());
            let min_height = parse_f64_field(c.get("minHeight")).unwrap_or(0.0);
            let max_height = parse_f64_field(c.get("maxHeight")).filter(|v| v.is_finite());
            let description = c
                .get("description")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| {
                    let tmp = LayoutConstraints {
                        constraint_type: constraint_type.clone(),
                        description: String::new(),
                        min_width,
                        max_width,
                        min_height,
                        max_height,
                    };
                    format!(
                        "{}({}, {})",
                        constraint_type,
                        tmp.format_width_range(),
                        tmp.format_height_range()
                    )
                });
            Some(LayoutConstraints {
                constraint_type,
                description,
                min_width,
                max_width,
                min_height,
                max_height,
            })
        });

        let is_box = node
            .get("isBox")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                constraints
                    .as_ref()
                    .is_some_and(|c| c.constraint_type.contains("Box"))
                    || size.is_some()
            });

        let flex_factor = node.get("flexFactor").and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                .and_then(|n| u32::try_from(n).ok())
        });

        let flex_fit = node
            .get("flexFit")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "null")
            .map(ToOwned::to_owned);

        let render_obj = node.get("renderObject");
        let render_object_description = render_obj
            .and_then(|ro| ro.get("description"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let mut direction = node
            .get("direction")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let mut main_axis_alignment = node
            .get("mainAxisAlignment")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let mut cross_axis_alignment = node
            .get("crossAxisAlignment")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let mut main_axis_size = node
            .get("mainAxisSize")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        for props_source in [
            render_obj.and_then(|ro| ro.get("properties")),
            node.get("properties"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(arr) = props_source.as_array() {
                for prop in arr {
                    let pname = prop.get("name").and_then(Value::as_str).unwrap_or("");
                    let pval = prop
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    if pval.is_empty() || pval == "null" {
                        continue;
                    }
                    match pname {
                        "direction" if direction.is_none() => direction = Some(pval.to_string()),
                        "mainAxisAlignment" if main_axis_alignment.is_none() => {
                            main_axis_alignment = Some(pval.to_string());
                        }
                        "crossAxisAlignment" if cross_axis_alignment.is_none() => {
                            cross_axis_alignment = Some(pval.to_string());
                        }
                        "mainAxisSize" if main_axis_size.is_none() => {
                            main_axis_size = Some(pval.to_string());
                        }
                        _ => {}
                    }
                }
            }
        }

        if direction.is_none() {
            if widget_runtime_type == "Row" || description.starts_with("Row") {
                direction = Some("horizontal".to_string());
            } else if widget_runtime_type == "Column" || description.starts_with("Column") {
                direction = Some("vertical".to_string());
            }
        }

        let is_flex = node
            .get("isFlex")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                matches!(widget_runtime_type.as_str(), "Row" | "Column" | "Flex")
                    || render_object_description
                        .as_deref()
                        .is_some_and(|d| d.contains("RenderFlex"))
                    || direction.is_some()
                    || main_axis_alignment.is_some()
            });

        let children: Vec<LayoutExplorerNode> = node
            .get("children")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Self::parse_node).collect())
            .unwrap_or_default();

        let direct_overflow = match (&size, &constraints) {
            (Some(sz), Some(c)) => {
                c.max_width.is_some_and(|max_w| sz.width > max_w + 0.5)
                    || c.max_height.is_some_and(|max_h| sz.height > max_h + 0.5)
            }
            _ => false,
        };
        let flex_children_overflow = match (&size, direction.as_deref()) {
            (Some(sz), Some("horizontal")) => {
                let sum_w: f64 = children
                    .iter()
                    .filter_map(|ch| ch.size.as_ref().map(|s| s.width))
                    .sum();
                sum_w > sz.width + 0.5
            }
            (Some(sz), Some("vertical")) => {
                let sum_h: f64 = children
                    .iter()
                    .filter_map(|ch| ch.size.as_ref().map(|s| s.height))
                    .sum();
                sum_h > sz.height + 0.5
            }
            _ => false,
        };
        let has_overflow = direct_overflow || flex_children_overflow;

        Some(Self {
            value_id,
            widget_runtime_type,
            description,
            is_box,
            size,
            constraints,
            is_flex,
            flex_factor,
            flex_fit,
            direction,
            main_axis_alignment,
            cross_axis_alignment,
            main_axis_size,
            render_object_description,
            has_overflow,
            children,
        })
    }
}

fn parse_f64_field(val: Option<&Value>) -> Option<f64> {
    let v = val?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    let s = v.as_str()?.trim();
    if s.eq_ignore_ascii_case("infinity")
        || s.eq_ignore_ascii_case("unconstrained")
        || s == "∞"
    {
        return Some(f64::INFINITY);
    }
    s.parse::<f64>().ok()
}

impl WidgetTreeNode {
    pub fn parse_from_json(v: &Value) -> Option<Self> {
        let root = v.get("result").unwrap_or(v);
        if root.is_null() {
            return None;
        }
        Self::parse_node(root, 0)
    }

    pub(crate) fn parse_node(node: &Value, depth: usize) -> Option<Self> {
        if node.is_null() || !node.is_object() {
            return None;
        }
        let description = node
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("Widget")
            .to_string();
        let widget_runtime_type = node
            .get("widgetRuntimeType")
            .and_then(Value::as_str)
            .unwrap_or(&description)
            .to_string();
        let value_id = node
            .get("valueId")
            .and_then(Value::as_str)
            .or_else(|| node.get("objectId").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();

        let created_by_local_project = node
            .get("createdByLocalProject")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let location = parse_creation_location(node.get("creationLocation"));

        let children: Vec<WidgetTreeNode> = node
            .get("children")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|child| Self::parse_node(child, depth + 1))
                    .collect()
            })
            .unwrap_or_default();

        let has_children = node
            .get("hasChildren")
            .and_then(Value::as_bool)
            .unwrap_or(!children.is_empty());

        let properties = parse_widget_properties(node);

        Some(Self {
            value_id,
            widget_runtime_type,
            description,
            has_children,
            created_by_local_project,
            location,
            properties,
            children,
            expanded: depth < 3 || created_by_local_project,
        })
    }
}

/// Parses the response of `ext.flutter.inspector.getChildrenSummaryTree` or `getChildrenDetailsSubtree`.
pub fn parse_widget_children_from_json(v: &Value) -> Vec<WidgetTreeNode> {
    let root = v.get("result").unwrap_or(v);
    let arr = root
        .as_array()
        .or_else(|| root.get("children").and_then(Value::as_array));
    let Some(items) = arr else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| WidgetTreeNode::parse_node(item, 1))
        .collect()
}

/// Constructs official Flutter Widget Inspector `addPubRootDirectories` / `setPubRootDirectories`
/// varargs map (`{ "arg0": "file:///...", "arg1": "file:///..." }`), including both the raw path
/// and its canonicalized path so macOS symlinks (e.g. `/var` vs `/private/var`) match seamlessly.
pub fn build_pub_root_directories_params(project_root: &Path) -> Value {
    let mut uris: Vec<String> = Vec::new();
    let to_file_uri = |p: &Path| -> String {
        url::Url::from_file_path(p)
            .map(|u| u.to_string().trim_end_matches('/').to_string())
            .unwrap_or_else(|_| format!("file://{}", p.display()))
    };

    uris.push(to_file_uri(project_root));
    if let Ok(canon) = std::fs::canonicalize(project_root) {
        let canon_uri = to_file_uri(&canon);
        if !uris.contains(&canon_uri) {
            uris.push(canon_uri);
        }
    }

    let mut obj = serde_json::Map::new();
    for (idx, uri) in uris.into_iter().enumerate() {
        obj.insert(format!("arg{idx}"), json!(uri));
    }
    Value::Object(obj)
}

fn parse_creation_location(loc: Option<&Value>) -> Option<SourceLocation> {
    let loc = loc?;
    let uri = loc
        .get("file")
        .and_then(Value::as_str)
        .or_else(|| loc.get("uri").and_then(Value::as_str))?
        .to_string();
    let line = loc
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(1);
    let column = loc
        .get("column")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(1);
    Some(SourceLocation { uri, line, column })
}

pub fn parse_widget_properties(v: &Value) -> Vec<WidgetProperty> {
    let root = v.get("result").unwrap_or(v);
    let arr = root
        .get("properties")
        .and_then(Value::as_array)
        .or_else(|| root.as_array());

    let Some(items) = arr else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() && description.is_empty() {
            continue;
        }
        let level = item
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("info")
            .to_string();
        if level == "hidden" {
            continue;
        }
        let property_type = item
            .get("propertyType")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let color_argb = extract_color_argb(item, &description);

        out.push(WidgetProperty {
            name: if name.is_empty() {
                "value".to_string()
            } else {
                name
            },
            value: description,
            level,
            property_type,
            color_argb,
        });
    }
    out
}

fn extract_color_argb(item: &Value, description: &str) -> Option<u32> {
    if let Some(props) = item.get("valueProperties").and_then(Value::as_object) {
        let r = props.get("red")?.as_u64()? as u32;
        let g = props.get("green")?.as_u64()? as u32;
        let b = props.get("blue")?.as_u64()? as u32;
        let a = props.get("alpha").and_then(Value::as_u64).unwrap_or(255) as u32;
        return Some(((a & 0xFF) << 24) | ((r & 0xFF) << 16) | ((g & 0xFF) << 8) | (b & 0xFF));
    }
    if let Some(idx) = description.find("0x") {
        let hex: String = description[idx + 2..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if hex.len() == 8 {
            return u32::from_str_radix(&hex, 16).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_root_widget_tree_children_and_pub_root_varargs() {
        let tree_json = json!({
            "result": {
                "description": "CounterPage",
                "widgetRuntimeType": "CounterPage",
                "valueId": "inspector-10",
                "createdByLocalProject": true,
                "hasChildren": true,
                "creationLocation": {
                    "file": "file:///Users/dev/app/lib/main.dart",
                    "line": 42,
                    "column": 7
                },
                "properties": [
                    {
                        "name": "color",
                        "description": "Color(0xff2196f3)",
                        "level": "info",
                        "valueProperties": {
                            "red": 33,
                            "green": 150,
                            "blue": 243,
                            "alpha": 255
                        }
                    }
                ],
                "children": []
            }
        });

        let root = WidgetTreeNode::parse_from_json(&tree_json).expect("parse root");
        assert_eq!(root.widget_runtime_type, "CounterPage");
        assert_eq!(root.value_id, "inspector-10");
        assert!(root.has_children);
        assert!(root.children.is_empty());
        assert_eq!(root.properties[0].color_argb, Some(0xff2196f3));

        // Lazy-loaded children via `getChildrenSummaryTree`
        let children_json = json!({
            "result": [
                {
                    "description": "Scaffold",
                    "widgetRuntimeType": "Scaffold",
                    "valueId": "inspector-11",
                    "createdByLocalProject": false,
                    "hasChildren": true
                },
                {
                    "description": "FloatingActionButton",
                    "widgetRuntimeType": "FloatingActionButton",
                    "valueId": "inspector-12",
                    "createdByLocalProject": true,
                    "hasChildren": false
                }
            ]
        });
        let children = parse_widget_children_from_json(&children_json);
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].value_id, "inspector-11");
        assert_eq!(children[1].widget_runtime_type, "FloatingActionButton");

        let params = build_pub_root_directories_params(Path::new("/Users/dev/app"));
        assert_eq!(params["arg0"], "file:///Users/dev/app");
    }

    #[test]
    fn parses_layout_explorer_node_flex_properties_and_detects_overflow() {
        let layout_json = json!({
            "result": {
                "valueId": "inspector-row-1",
                "widgetRuntimeType": "Row",
                "description": "Row",
                "isBox": true,
                "isFlex": true,
                "size": { "width": "360.0", "height": "56.0" },
                "constraints": {
                    "type": "BoxConstraints",
                    "description": "BoxConstraints(0.0<=w<=360.0, 0.0<=h<=Infinity)",
                    "minWidth": "0.0",
                    "maxWidth": "360.0",
                    "minHeight": "0.0",
                    "maxHeight": "Infinity"
                },
                "renderObject": {
                    "description": "RenderFlex#a1b2c",
                    "properties": [
                        { "name": "direction", "description": "horizontal" },
                        { "name": "mainAxisAlignment", "description": "spaceBetween" },
                        { "name": "crossAxisAlignment", "description": "center" },
                        { "name": "mainAxisSize", "description": "max" }
                    ]
                },
                "children": [
                    {
                        "valueId": "inspector-child-1",
                        "widgetRuntimeType": "Expanded",
                        "description": "Expanded",
                        "flexFactor": 2,
                        "flexFit": "tight",
                        "size": { "width": "240.0", "height": "56.0" },
                        "constraints": {
                            "type": "BoxConstraints",
                            "minWidth": "240.0",
                            "maxWidth": "240.0",
                            "minHeight": "0.0",
                            "maxHeight": "56.0"
                        }
                    },
                    {
                        "valueId": "inspector-child-2",
                        "widgetRuntimeType": "Container",
                        "description": "Container",
                        "flexFactor": "1",
                        "flexFit": "loose",
                        "size": { "width": "180.0", "height": "56.0" }
                    }
                ]
            }
        });

        let node = LayoutExplorerNode::parse_from_json(&layout_json).expect("parse layout node");
        assert_eq!(node.value_id, "inspector-row-1");
        assert!(node.is_box);
        assert!(node.is_flex);
        assert_eq!(node.direction.as_deref(), Some("horizontal"));
        assert_eq!(node.main_axis_alignment.as_deref(), Some("spaceBetween"));
        assert_eq!(node.cross_axis_alignment.as_deref(), Some("center"));
        assert_eq!(node.main_axis_size.as_deref(), Some("max"));
        assert_eq!(node.size.as_ref().unwrap().width, 360.0);
        assert_eq!(node.constraints.as_ref().unwrap().max_width, Some(360.0));
        assert_eq!(node.constraints.as_ref().unwrap().max_height, None);
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].flex_factor, Some(2));
        assert_eq!(node.children[0].flex_fit.as_deref(), Some("tight"));
        assert_eq!(node.children[1].flex_factor, Some(1));
        assert_eq!(node.children[1].flex_fit.as_deref(), Some("loose"));
        // 240.0 + 180.0 = 420.0 > 360.0 -> overflow detected!
        assert!(node.has_overflow);
    }
}
