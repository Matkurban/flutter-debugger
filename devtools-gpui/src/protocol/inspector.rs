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
}
