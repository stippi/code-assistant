//! The page as an accessibility tree: what the model reads to understand a page
//! and to target elements by `ref_N` instead of guessing CSS selectors.
//!
//! The tree comes from CDP `Accessibility.getFullAXTree`; rendering it is pure
//! (see [`render`]), so it is tested without a browser. Every printed node that
//! has a DOM node gets a `ref_N`, mapped to its `backendDOMNodeId` by
//! [`RefMap`] — stable for as long as the document lives.

use serde::Deserialize;
use std::collections::HashMap;

/// Raw `Accessibility.getFullAXTree`. chromiumoxide's typed bindings would fail
/// on any property name newer than its bundled protocol, so the response is
/// read leniently.
#[derive(serde::Serialize)]
pub(crate) struct GetFullAxTreeRaw {}

impl chromiumoxide::Method for GetFullAxTreeRaw {
    fn identifier(&self) -> chromiumoxide::types::MethodId {
        "Accessibility.getFullAXTree".into()
    }
}

impl chromiumoxide::Command for GetFullAxTreeRaw {
    type Response = RawAxTree;
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAxTree {
    #[serde(default)]
    pub nodes: Vec<RawAxNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawAxNode {
    pub node_id: String,
    #[serde(default)]
    pub ignored: bool,
    #[serde(default)]
    pub role: Option<RawAxValue>,
    #[serde(default)]
    pub name: Option<RawAxValue>,
    #[serde(default)]
    pub value: Option<RawAxValue>,
    #[serde(default)]
    pub properties: Vec<RawAxProperty>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub child_ids: Vec<String>,
    #[serde(default, rename = "backendDOMNodeId")]
    pub backend_dom_node_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAxValue {
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAxProperty {
    pub name: String,
    pub value: RawAxValue,
}

impl RawAxValue {
    fn text(&self) -> String {
        match &self.value {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }
}

impl RawAxNode {
    fn role(&self) -> String {
        self.role.as_ref().map(RawAxValue::text).unwrap_or_default()
    }

    fn name(&self) -> String {
        self.name.as_ref().map(RawAxValue::text).unwrap_or_default()
    }

    fn property(&self, name: &str) -> Option<&RawAxValue> {
        self.properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.value)
    }
}

/// `ref_N` handles for DOM nodes, stable across reads of the same document.
#[derive(Default)]
pub(crate) struct RefMap {
    by_backend: HashMap<i64, String>,
    by_ref: HashMap<String, i64>,
    next: u32,
}

impl RefMap {
    pub fn ref_for(&mut self, backend_id: i64) -> String {
        if let Some(r) = self.by_backend.get(&backend_id) {
            return r.clone();
        }
        self.next += 1;
        let r = format!("ref_{}", self.next);
        self.by_backend.insert(backend_id, r.clone());
        self.by_ref.insert(r.clone(), backend_id);
        r
    }

    pub fn backend_id(&self, r: &str) -> Option<i64> {
        self.by_ref.get(r).copied()
    }
}

/// Roles a user acts on; `filter: interactive` keeps only these.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "link",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "scrollbar",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "treeitem",
];

/// Roles that only structure the tree; without a name they are not printed
/// and their children move up a level.
const TRANSPARENT_ROLES: &[&str] = &["generic", "none", "presentation", "LineBreak"];

/// How to render a snapshot.
pub(crate) struct RenderOptions<'a> {
    pub interactive_only: bool,
    /// Print only the subtree under this ref.
    pub root_ref: Option<&'a str>,
    pub max_depth: usize,
}

/// Render the tree as YAML-style lines, `- role "name" [ref_N] attrs`,
/// indented by depth. With `interactive_only` the result is a flat list of
/// actionable elements.
pub(crate) fn render(
    nodes: &[RawAxNode],
    refs: &mut RefMap,
    opts: &RenderOptions,
) -> Result<Vec<String>, String> {
    let by_id: HashMap<&str, &RawAxNode> = nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let start: Vec<&RawAxNode> = match opts.root_ref {
        Some(r) => {
            let backend = refs
                .backend_id(r)
                .ok_or_else(|| format!("unknown ref '{r}' (read the page again)"))?;
            let node = nodes
                .iter()
                .find(|n| n.backend_dom_node_id == Some(backend))
                .ok_or_else(|| format!("{r} is no longer in the page (read the page again)"))?;
            vec![node]
        }
        None => nodes.iter().filter(|n| n.parent_id.is_none()).collect(),
    };
    let mut lines = Vec::new();
    for node in start {
        walk(node, &by_id, refs, opts, 0, None, &mut lines);
    }
    Ok(lines)
}

fn walk(
    node: &RawAxNode,
    by_id: &HashMap<&str, &RawAxNode>,
    refs: &mut RefMap,
    opts: &RenderOptions,
    depth: usize,
    parent_name: Option<&str>,
    lines: &mut Vec<String>,
) {
    let role = node.role();
    let name = node.name();
    let skip_self = node.ignored
        || role == "InlineTextBox"
        || (TRANSPARENT_ROLES.contains(&role.as_str()) && name.is_empty())
        // Text that only repeats its parent's name (a link's or button's
        // label) adds nothing.
        || (role == "StaticText" && (name.trim().is_empty() || Some(name.as_str()) == parent_name));
    let print =
        !skip_self && (!opts.interactive_only || INTERACTIVE_ROLES.contains(&role.as_str()));

    let child_depth = if skip_self || opts.interactive_only {
        depth
    } else {
        depth + 1
    };
    if print && depth < opts.max_depth {
        lines.push(line(
            node,
            &role,
            &name,
            refs,
            if opts.interactive_only { 0 } else { depth },
        ));
    }
    if child_depth > opts.max_depth {
        return;
    }
    let next_parent_name = if skip_self {
        parent_name
    } else {
        Some(name.as_str())
    };
    for child in node
        .child_ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()))
    {
        walk(
            child,
            by_id,
            refs,
            opts,
            child_depth,
            next_parent_name,
            lines,
        );
    }
}

fn line(node: &RawAxNode, role: &str, name: &str, refs: &mut RefMap, depth: usize) -> String {
    let role = match role {
        "RootWebArea" => "document",
        "StaticText" => "text",
        other => other,
    };
    let mut out = format!("{}- {role}", "  ".repeat(depth));
    if !name.is_empty() {
        out.push_str(&format!(" {}", quote(name, 100)));
    }
    if let Some(backend) = node.backend_dom_node_id {
        out.push_str(&format!(" [{}]", refs.ref_for(backend)));
    }
    let value = node
        .value
        .as_ref()
        .map(RawAxValue::text)
        .unwrap_or_default();
    if !value.is_empty() && value != name {
        out.push_str(&format!(" value={}", quote(&value, 100)));
    }
    for (prop, label) in [
        ("checked", "checked"),
        ("pressed", "pressed"),
        ("selected", "selected"),
        ("expanded", "expanded"),
        ("disabled", "disabled"),
        ("required", "required"),
        ("invalid", "invalid"),
        ("focused", "focused"),
        ("level", "level"),
    ] {
        let Some(v) = node.property(prop) else {
            continue;
        };
        match v.value.as_ref() {
            Some(serde_json::Value::Bool(true)) => out.push_str(&format!(" {label}")),
            Some(serde_json::Value::Bool(false)) | None => {
                // `checked`/`expanded`/`pressed` false is state worth seeing.
                if matches!(prop, "checked" | "expanded" | "pressed") {
                    out.push_str(&format!(" {label}=false"));
                }
            }
            Some(serde_json::Value::String(s)) if s == "false" => {
                if matches!(prop, "checked" | "expanded" | "pressed") {
                    out.push_str(&format!(" {label}=false"));
                }
            }
            Some(serde_json::Value::String(s)) if s == "true" => out.push_str(&format!(" {label}")),
            Some(other) => out.push_str(&format!(
                " {label}={}",
                RawAxValue {
                    value: Some(other.clone())
                }
                .text()
            )),
        }
    }
    if role == "link"
        && let Some(url) = node.property("url").map(RawAxValue::text)
        && !url.is_empty()
    {
        out.push_str(&format!(" href={}", quote(&url, 120)));
    }
    out
}

fn quote(s: &str, max_chars: usize) -> String {
    let s = s.replace('\n', " ");
    let s = if s.chars().count() > max_chars {
        let cut: String = s.chars().take(max_chars).collect();
        format!("{cut}…")
    } else {
        s
    };
    format!("{s:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document with a heading, a link whose text node repeats its name, a
    /// nameless wrapper div, a filled textbox, and an unchecked checkbox.
    fn sample() -> Vec<RawAxNode> {
        let json = serde_json::json!({"nodes": [
            {"nodeId": "1", "role": {"value": "RootWebArea"}, "name": {"value": "Demo"},
             "childIds": ["2", "3", "5"], "backendDOMNodeId": 10},
            {"nodeId": "2", "parentId": "1", "role": {"value": "heading"}, "name": {"value": "Welcome"},
             "properties": [{"name": "level", "value": {"value": 1}}], "backendDOMNodeId": 11},
            {"nodeId": "3", "parentId": "1", "role": {"value": "link"}, "name": {"value": "Docs"},
             "properties": [{"name": "url", "value": {"value": "https://x.test/docs"}}],
             "childIds": ["4"], "backendDOMNodeId": 12},
            {"nodeId": "4", "parentId": "3", "role": {"value": "StaticText"}, "name": {"value": "Docs"},
             "backendDOMNodeId": 13},
            {"nodeId": "5", "parentId": "1", "role": {"value": "generic"}, "name": {"value": ""},
             "childIds": ["6", "7", "8"], "backendDOMNodeId": 14},
            {"nodeId": "6", "parentId": "5", "role": {"value": "textbox"}, "name": {"value": "User"},
             "value": {"value": "stephan"}, "properties": [{"name": "focused", "value": {"value": true}}],
             "backendDOMNodeId": 15},
            {"nodeId": "7", "parentId": "5", "role": {"value": "checkbox"}, "name": {"value": "Remember"},
             "properties": [{"name": "checked", "value": {"value": "false"}}], "backendDOMNodeId": 16},
            {"nodeId": "8", "parentId": "5", "ignored": true, "role": {"value": "none"}, "backendDOMNodeId": 17}
        ]});
        serde_json::from_value::<RawAxTree>(json).unwrap().nodes
    }

    fn opts() -> RenderOptions<'static> {
        RenderOptions {
            interactive_only: false,
            root_ref: None,
            max_depth: 15,
        }
    }

    #[test]
    fn renders_an_indented_tree_with_refs_and_states() {
        let mut refs = RefMap::default();
        let lines = render(&sample(), &mut refs, &opts()).unwrap();
        assert_eq!(
            lines,
            vec![
                r#"- document "Demo" [ref_1]"#,
                r#"  - heading "Welcome" [ref_2] level=1"#,
                r#"  - link "Docs" [ref_3] href="https://x.test/docs""#,
                r#"  - textbox "User" [ref_4] value="stephan" focused"#,
                r#"  - checkbox "Remember" [ref_5] checked=false"#,
            ]
        );
    }

    #[test]
    fn interactive_filter_is_a_flat_list_of_actionable_elements() {
        let mut refs = RefMap::default();
        let lines = render(
            &sample(),
            &mut refs,
            &RenderOptions {
                interactive_only: true,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(
            lines,
            vec![
                r#"- link "Docs" [ref_1] href="https://x.test/docs""#,
                r#"- textbox "User" [ref_2] value="stephan" focused"#,
                r#"- checkbox "Remember" [ref_3] checked=false"#,
            ]
        );
    }

    #[test]
    fn refs_are_stable_across_reads_and_scope_a_subtree() {
        let mut refs = RefMap::default();
        render(&sample(), &mut refs, &opts()).unwrap();
        // The link keeps its ref on a second read and roots a subtree.
        let lines = render(
            &sample(),
            &mut refs,
            &RenderOptions {
                root_ref: Some("ref_3"),
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(
            lines,
            vec![r#"- link "Docs" [ref_3] href="https://x.test/docs""#]
        );
        assert!(
            render(
                &sample(),
                &mut refs,
                &RenderOptions {
                    root_ref: Some("ref_99"),
                    ..opts()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn depth_limits_the_tree() {
        let mut refs = RefMap::default();
        let lines = render(
            &sample(),
            &mut refs,
            &RenderOptions {
                max_depth: 1,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(lines, vec![r#"- document "Demo" [ref_1]"#]);
    }
}
