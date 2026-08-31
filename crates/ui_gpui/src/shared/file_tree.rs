//! Nested, collapsible tree of changed files for the Review panel.
//!
//! [`build_tree`] is a pure function (unit-tested) that turns a flat list of
//! [`git::ChangedFile`]s into a nested [`TreeNode`] structure. [`ChangedFilesTree`]
//! is the GPUI entity that renders it, tracks expansion/selection state, and
//! emits [`ChangedFilesTreeEvent::FileSelected`] when the user clicks a file.

use crate::shared::file_icons;
use git::{ChangeStatus, ChangedFile};
use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, Render, Window, div, prelude::*, px, svg,
};
use gpui_component::ActiveTheme;
use std::collections::HashSet;

/// A node in the changed-files tree: either a directory (with children) or a
/// leaf file carrying its change status.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeNode {
    Dir {
        /// Last path segment (display name).
        name: String,
        /// Full relative path from the repo root (unique key).
        path: String,
        children: Vec<TreeNode>,
    },
    File {
        name: String,
        path: String,
        status: ChangeStatus,
    },
}

/// Build a nested tree from a flat list of changed files.
///
/// Paths are split on `/`; files sharing a directory prefix are merged under a
/// single directory node. Siblings are sorted directories-first, then
/// case-insensitively by name.
pub fn build_tree(files: &[ChangedFile]) -> Vec<TreeNode> {
    let mut roots: Vec<TreeNode> = Vec::new();
    for f in files {
        insert_path(&mut roots, &f.path, f.status, "");
    }
    sort_nodes(&mut roots);
    roots
}

fn insert_path(nodes: &mut Vec<TreeNode>, rel: &str, status: ChangeStatus, prefix: &str) {
    let mut parts = rel.splitn(2, '/');
    let head = match parts.next() {
        Some(h) if !h.is_empty() => h,
        // Empty or leading-slash segment: skip it.
        _ => return,
    };
    let rest = parts.next();
    let full = if prefix.is_empty() {
        head.to_string()
    } else {
        format!("{prefix}/{head}")
    };

    match rest {
        None => {
            nodes.push(TreeNode::File {
                name: head.to_string(),
                path: full,
                status,
            });
        }
        Some(rest) => {
            let idx = nodes
                .iter()
                .position(|n| matches!(n, TreeNode::Dir { name, .. } if name == head));
            let idx = match idx {
                Some(i) => i,
                None => {
                    nodes.push(TreeNode::Dir {
                        name: head.to_string(),
                        path: full.clone(),
                        children: Vec::new(),
                    });
                    nodes.len() - 1
                }
            };
            if let TreeNode::Dir { children, .. } = &mut nodes[idx] {
                insert_path(children, rest, status, &full);
            }
        }
    }
}

fn node_name(node: &TreeNode) -> &str {
    match node {
        TreeNode::Dir { name, .. } | TreeNode::File { name, .. } => name,
    }
}

fn sort_nodes(nodes: &mut [TreeNode]) {
    nodes.sort_by(|a, b| {
        // Directories sort before files.
        let rank = |n: &TreeNode| matches!(n, TreeNode::File { .. }) as u8;
        rank(a).cmp(&rank(b)).then_with(|| {
            node_name(a)
                .to_lowercase()
                .cmp(&node_name(b).to_lowercase())
        })
    });
    for n in nodes.iter_mut() {
        if let TreeNode::Dir { children, .. } = n {
            sort_nodes(children);
        }
    }
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

/// Events emitted by [`ChangedFilesTree`].
#[derive(Clone, Debug)]
pub enum ChangedFilesTreeEvent {
    /// The user selected a file. Carries the file's repo-relative path.
    FileSelected(String),
}

/// A flattened, renderable row (computed each render from the tree + expansion).
struct Row {
    depth: usize,
    is_dir: bool,
    name: String,
    path: String,
    status: Option<ChangeStatus>,
    expanded: bool,
}

/// Nested collapsible tree of changed files.
pub struct ChangedFilesTree {
    nodes: Vec<TreeNode>,
    /// Full paths of directories that are currently expanded.
    expanded: HashSet<String>,
    /// Currently selected file path.
    selected: Option<String>,
    focus_handle: FocusHandle,
}

impl EventEmitter<ChangedFilesTreeEvent> for ChangedFilesTree {}

impl ChangedFilesTree {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            nodes: Vec::new(),
            expanded: HashSet::new(),
            selected: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Replace the file list, rebuilding the tree. All directories start
    /// expanded. Preserves the current selection if it still exists.
    pub fn set_files(&mut self, files: &[ChangedFile], cx: &mut Context<Self>) {
        self.nodes = build_tree(files);
        self.expanded.clear();
        collect_dir_paths(&self.nodes, &mut self.expanded);
        // Drop selection if the selected file is gone.
        if let Some(sel) = &self.selected
            && !files.iter().any(|f| &f.path == sel)
        {
            self.selected = None;
        }
        cx.notify();
    }

    /// Set the selected file path (without emitting an event).
    pub fn set_selected(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        self.selected = path;
        cx.notify();
    }

    /// The currently selected file path, if any.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_string());
        }
        cx.notify();
    }

    fn on_file_click(&mut self, path: String, cx: &mut Context<Self>) {
        self.selected = Some(path.clone());
        cx.notify();
        cx.emit(ChangedFilesTreeEvent::FileSelected(path));
    }

    /// Walk the tree honoring expansion state, producing a flat list of rows.
    fn visible_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.push_rows(&self.nodes, 0, &mut rows);
        rows
    }

    fn push_rows(&self, nodes: &[TreeNode], depth: usize, out: &mut Vec<Row>) {
        for node in nodes {
            match node {
                TreeNode::Dir {
                    name,
                    path,
                    children,
                } => {
                    let expanded = self.expanded.contains(path);
                    out.push(Row {
                        depth,
                        is_dir: true,
                        name: name.clone(),
                        path: path.clone(),
                        status: None,
                        expanded,
                    });
                    if expanded {
                        self.push_rows(children, depth + 1, out);
                    }
                }
                TreeNode::File { name, path, status } => {
                    out.push(Row {
                        depth,
                        is_dir: false,
                        name: name.clone(),
                        path: path.clone(),
                        status: Some(*status),
                        expanded: false,
                    });
                }
            }
        }
    }
}

/// Recursively collect the full paths of all directory nodes.
fn collect_dir_paths(nodes: &[TreeNode], out: &mut HashSet<String>) {
    for n in nodes {
        if let TreeNode::Dir { path, children, .. } = n {
            out.insert(path.clone());
            collect_dir_paths(children, out);
        }
    }
}

/// Single-letter badge and color for a change status.
fn status_badge(status: ChangeStatus) -> (&'static str, gpui::Hsla) {
    match status {
        ChangeStatus::Added | ChangeStatus::Untracked => ("A", gpui::rgb(0x3f_a5_5a).into()),
        ChangeStatus::Modified => ("M", gpui::rgb(0xc7_9a_3a).into()),
        ChangeStatus::Deleted => ("D", gpui::rgb(0xc7_4a_4a).into()),
        ChangeStatus::Renamed => ("R", gpui::rgb(0x4a_82_c7).into()),
        ChangeStatus::Copied => ("C", gpui::rgb(0x4a_82_c7).into()),
        ChangeStatus::TypeChanged => ("T", gpui::rgb(0x8a_6a_c7).into()),
    }
}

impl Focusable for ChangedFilesTree {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChangedFilesTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.visible_rows();
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let accent = cx.theme().accent;
        let selected = self.selected.clone();

        if rows.is_empty() {
            return div()
                .p_3()
                .text_sm()
                .text_color(muted)
                .child("No changes")
                .into_any_element();
        }

        div()
            .flex()
            .flex_col()
            .children(rows.into_iter().map(|row| {
                let indent = px(8.0 + row.depth as f32 * 12.0);
                let is_selected = !row.is_dir && selected.as_deref() == Some(row.path.as_str());
                let row_path = row.path.clone();

                let mut container = div()
                    .id(gpui::SharedString::from(format!("tree-{}", row.path)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .pl(indent)
                    .pr_2()
                    .py_0p5()
                    .text_sm()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().muted));

                if is_selected {
                    container = container.bg(accent);
                }

                // Leading glyph: chevron + folder for dirs, spacer + file icon for files.
                if row.is_dir {
                    let chevron = if row.expanded {
                        "icons/chevron_down.svg"
                    } else {
                        "icons/chevron_right.svg"
                    };
                    let folder = if row.expanded {
                        "icons/file_icons/folder_open.svg"
                    } else {
                        "icons/file_icons/folder.svg"
                    };
                    container = container
                        .child(svg().size(px(12.)).path(chevron).text_color(muted))
                        .child(svg().size(px(14.)).path(folder).text_color(muted))
                        .child(div().text_color(fg).child(row.name.clone()));
                } else {
                    let (badge, badge_color) = row.status.map(status_badge).unwrap_or((" ", muted));
                    let icon = file_icons::get().get_icon_for_filename(&row.name);
                    container = container
                        // Align file rows under the folder glyph (skip chevron slot).
                        .child(div().size(px(12.)))
                        .child(file_icons::render_icon(&icon, 14.0, muted, "📄"))
                        .child(div().flex_1().text_color(fg).child(row.name.clone()))
                        .child(
                            div()
                                .w(px(14.))
                                .flex()
                                .justify_center()
                                .text_color(badge_color)
                                .child(badge),
                        );
                }

                let is_dir = row.is_dir;
                container.on_click(cx.listener(move |this, _ev, _window, cx| {
                    if is_dir {
                        this.toggle_dir(&row_path, cx);
                    } else {
                        this.on_file_click(row_path.clone(), cx);
                    }
                }))
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, status: ChangeStatus) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            orig_path: None,
            status,
        }
    }

    #[test]
    fn build_tree_nests_and_sorts_dirs_first() {
        let files = vec![
            file("z.rs", ChangeStatus::Modified),
            file("a/c.rs", ChangeStatus::Added),
            file("a/b.rs", ChangeStatus::Modified),
            file("a/sub/d.rs", ChangeStatus::Deleted),
        ];
        let tree = build_tree(&files);

        // Root: dir "a" first, then file "z.rs".
        assert_eq!(tree.len(), 2);
        match &tree[0] {
            TreeNode::Dir {
                name,
                path,
                children,
            } => {
                assert_eq!(name, "a");
                assert_eq!(path, "a");
                // Inside "a": dir "sub" first, then files b.rs, c.rs (sorted).
                assert_eq!(children.len(), 3);
                assert!(matches!(&children[0], TreeNode::Dir { name, .. } if name == "sub"));
                assert!(matches!(&children[1], TreeNode::File { name, .. } if name == "b.rs"));
                assert!(matches!(&children[2], TreeNode::File { name, .. } if name == "c.rs"));
                // Nested file path is fully qualified.
                if let TreeNode::Dir { children: sub, .. } = &children[0] {
                    assert!(matches!(&sub[0], TreeNode::File { path, .. } if path == "a/sub/d.rs"));
                }
            }
            other => panic!("expected dir 'a', got {other:?}"),
        }
        assert!(matches!(&tree[1], TreeNode::File { name, .. } if name == "z.rs"));
    }

    #[test]
    fn build_tree_merges_shared_prefix() {
        let files = vec![
            file("src/main.rs", ChangeStatus::Modified),
            file("src/lib.rs", ChangeStatus::Modified),
        ];
        let tree = build_tree(&files);
        assert_eq!(tree.len(), 1);
        match &tree[0] {
            TreeNode::Dir { name, children, .. } => {
                assert_eq!(name, "src");
                assert_eq!(children.len(), 2);
            }
            other => panic!("expected single 'src' dir, got {other:?}"),
        }
    }

    #[test]
    fn collect_dir_paths_gathers_all_dirs() {
        let files = vec![
            file("a/b/c.rs", ChangeStatus::Modified),
            file("d.rs", ChangeStatus::Added),
        ];
        let tree = build_tree(&files);
        let mut dirs = HashSet::new();
        collect_dir_paths(&tree, &mut dirs);
        assert!(dirs.contains("a"));
        assert!(dirs.contains("a/b"));
        assert_eq!(dirs.len(), 2);
    }
}
